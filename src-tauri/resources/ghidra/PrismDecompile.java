// 反编译单个函数，把伪 C 写到指定文件。
//
// 用法（由 analyzeHeadless 的 -postScript 调用）：
//
//     PrismDecompile.java <va|函数名> <输出 .c>
//
// `va` 接受 `0x401000` / 十进制；不是数字就按函数名找（精确匹配，
// 也匹配带命名空间的 `Class::method`）。单函数超时 60s，失败把 Ghidra
// 的原话打到 stderr——"low-level error" 也原样给，不翻译成"反编译失败"。
//
// @category Prism

import java.io.File;
import java.io.FileWriter;
import java.io.Writer;

import ghidra.app.decompiler.DecompInterface;
import ghidra.app.decompiler.DecompileOptions;
import ghidra.app.decompiler.DecompileResults;
import ghidra.app.script.GhidraScript;
import ghidra.program.model.address.Address;
import ghidra.program.model.listing.Function;
import ghidra.program.model.listing.FunctionIterator;
import ghidra.program.model.listing.FunctionManager;

public class PrismDecompile extends GhidraScript {

    /** 单函数反编译上限：超过这个时间基本是函数太大或反编译器卡住。 */
    private static final int TIMEOUT_SECONDS = 60;

    @Override
    public void run() throws Exception {
        String[] args = getScriptArgs();
        if (args.length < 2) {
            throw new IllegalArgumentException("用法：PrismDecompile.java <va|函数名> <输出 .c>");
        }
        String target = args[0].trim();
        File out = new File(args[1]);
        File parent = out.getParentFile();
        if (parent != null) {
            parent.mkdirs();
        }

        Function function = findFunction(target);
        if (function == null) {
            throw new IllegalArgumentException("没有这个函数：" + target);
        }

        DecompInterface decompiler = new DecompInterface();
        decompiler.setOptions(new DecompileOptions());
        if (!decompiler.openProgram(currentProgram)) {
            throw new IllegalStateException("反编译器起不来：" + decompiler.getLastMessage());
        }
        try {
            DecompileResults results =
                decompiler.decompileFunction(function, TIMEOUT_SECONDS, monitor);
            if (results == null) {
                throw new IllegalStateException("反编译没有返回结果：" + decompiler.getLastMessage());
            }
            if (!results.decompileCompleted()) {
                String error = results.getErrorMessage();
                if (error == null || error.isEmpty()) {
                    error = decompiler.getLastMessage();
                }
                throw new IllegalStateException("反编译失败：" + error);
            }
            if (results.getDecompiledFunction() == null) {
                throw new IllegalStateException("反编译完成但没有伪 C：" + decompiler.getLastMessage());
            }
            String code = results.getDecompiledFunction().getC();
            try (Writer writer = new FileWriter(out)) {
                writer.write(code);
            }
            println("PrismDecompile: " + function.getName() + " @ "
                + function.getEntryPoint() + " -> " + out.getAbsolutePath());
            // 这一行由 Rust 侧解析，用来回填函数的真实名字/地址/大小。
            println("PRISM_FUNCTION\t" + function.getEntryPoint().getOffset()
                + "\t" + function.getName() + "\t" + function.getBody().getNumAddresses());
        } finally {
            decompiler.dispose();
        }
    }

    private Function findFunction(String target) {
        FunctionManager manager = currentProgram.getFunctionManager();
        try {
            long value;
            if (target.startsWith("0x") || target.startsWith("0X")) {
                value = Long.parseUnsignedLong(target.substring(2), 16);
            } else {
                value = Long.parseUnsignedLong(target, 10);
            }
            Address address =
                currentProgram.getAddressFactory().getDefaultAddressSpace().getAddress(value);
            Function exact = manager.getFunctionAt(address);
            if (exact != null) {
                return exact;
            }
            return manager.getFunctionContaining(address);
        } catch (NumberFormatException notAnAddress) {
            // 不是数字：按函数名找。
        }

        FunctionIterator functions = manager.getFunctions(true);
        while (functions.hasNext()) {
            Function function = functions.next();
            if (function.getName().equals(target) || function.getName(true).equals(target)) {
                return function;
            }
        }
        return null;
    }
}
