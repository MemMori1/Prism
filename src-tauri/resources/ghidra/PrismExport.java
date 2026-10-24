// 把当前程序的函数边界、基本块与外部符号导出成 JSON。
//
// 这是 Prism 的 Ghidra 导出脚本：只做边界，不做反编译（伪 C 走 PrismDecompile.java）。
// 用法（由 analyzeHeadless 的 -postScript 调用）：
//
//     PrismExport.java <输出 functions.json>
//
// 输出形状与 Rust 侧 `backends/index.rs` 的 `GhidraExport` 对应：
// `{ functions: [{va,name,size,kind,blocks:[{va,size,succ}]}], imports:[{name,library}],
//    truncated, imageBase }`。
//
// @category Prism

import java.io.File;
import java.io.FileWriter;
import java.io.Writer;

import ghidra.app.script.GhidraScript;
import ghidra.program.model.block.BasicBlockModel;
import ghidra.program.model.block.CodeBlock;
import ghidra.program.model.block.CodeBlockIterator;
import ghidra.program.model.block.CodeBlockReference;
import ghidra.program.model.block.CodeBlockReferenceIterator;
import ghidra.program.model.listing.Function;
import ghidra.program.model.listing.FunctionIterator;
import ghidra.program.model.symbol.Symbol;
import ghidra.program.model.symbol.SymbolIterator;

public class PrismExport extends GhidraScript {

    @Override
    public void run() throws Exception {
        String[] args = getScriptArgs();
        if (args.length < 1) {
            throw new IllegalArgumentException("用法：PrismExport.java <输出 functions.json>");
        }
        File out = new File(args[0]);
        File parent = out.getParentFile();
        if (parent != null) {
            parent.mkdirs();
        }

        BasicBlockModel blockModel = new BasicBlockModel(currentProgram);
        StringBuilder json = new StringBuilder(1 << 20);

        json.append("{\"functions\":[");
        FunctionIterator functions = currentProgram.getFunctionManager().getFunctions(true);
        boolean firstFunction = true;
        int functionCount = 0;
        while (functions.hasNext()) {
            if (monitor.isCancelled()) {
                break;
            }
            Function function = functions.next();
            if (!firstFunction) {
                json.append(',');
            }
            firstFunction = false;
            functionCount++;

            json.append("{\"va\":").append(function.getEntryPoint().getOffset());
            json.append(",\"name\":\"").append(escape(function.getName())).append('"');
            json.append(",\"size\":").append(function.getBody().getNumAddresses());
            json.append(",\"kind\":\"").append(function.isThunk() ? "thunk" : "normal").append('"');
            json.append(",\"blocks\":[");

            CodeBlockIterator blocks =
                blockModel.getCodeBlocksContaining(function.getBody(), monitor);
            boolean firstBlock = true;
            while (blocks.hasNext()) {
                CodeBlock block = blocks.next();
                if (!firstBlock) {
                    json.append(',');
                }
                firstBlock = false;
                json.append("{\"va\":").append(block.getFirstStartAddress().getOffset());
                json.append(",\"size\":").append(block.getNumAddresses());
                json.append(",\"succ\":[");

                CodeBlockReferenceIterator destinations = block.getDestinations(monitor);
                boolean firstDestination = true;
                while (destinations.hasNext()) {
                    CodeBlockReference reference = destinations.next();
                    if (!firstDestination) {
                        json.append(',');
                    }
                    firstDestination = false;
                    json.append(reference.getDestinationAddress().getOffset());
                }
                json.append("]}");
            }
            json.append("]}");
        }
        json.append(']');

        json.append(",\"imports\":[");
        SymbolIterator externals = currentProgram.getSymbolTable().getExternalSymbols();
        boolean firstImport = true;
        while (externals.hasNext()) {
            Symbol symbol = externals.next();
            if (!firstImport) {
                json.append(',');
            }
            firstImport = false;
            json.append("{\"name\":\"").append(escape(symbol.getName())).append('"');
            String library = "";
            if (symbol.getParentNamespace() != null) {
                library = symbol.getParentNamespace().getName();
            }
            if (library != null && !library.isEmpty() && !"Global".equals(library)) {
                json.append(",\"library\":\"").append(escape(library)).append('"');
            }
            json.append('}');
        }
        json.append(']');

        // 脚本自己不做截断：上限由 Rust 侧按 MAX_FUNCTIONS 处理。
        // 这里写 false，超时/被截断由调用方改写。
        json.append(",\"truncated\":false");
        json.append(",\"imageBase\":").append(currentProgram.getImageBase().getOffset());
        json.append('}');

        try (Writer writer = new FileWriter(out)) {
            writer.write(json.toString());
        }
        println("PrismExport: " + functionCount + " functions -> " + out.getAbsolutePath());
    }

    /** 手写 JSON 转义：Ghidra 脚本里不引第三方库。 */
    private String escape(String text) {
        if (text == null) {
            return "";
        }
        StringBuilder out = new StringBuilder(text.length() + 8);
        for (int index = 0; index < text.length(); index++) {
            char ch = text.charAt(index);
            switch (ch) {
                case '"':
                    out.append("\\\"");
                    break;
                case '\\':
                    out.append("\\\\");
                    break;
                case '\n':
                    out.append("\\n");
                    break;
                case '\r':
                    out.append("\\r");
                    break;
                case '\t':
                    out.append("\\t");
                    break;
                default:
                    if (ch < 0x20) {
                        out.append(String.format("\\u%04x", (int) ch));
                    } else {
                        out.append(ch);
                    }
            }
        }
        return out.toString();
    }
}
