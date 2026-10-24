//! 配对二维码：把配对 URL 渲染成内联 SVG 字符串。
//!
//! 用白底黑模块——二维码对比度是识别率的底线，页面主题再暗也不给扫码环节
//! 玩配色。输出单条 `<path>`，体积小，前端直接 innerHTML。

/// 生成 SVG（`width`/`height` 由前端 CSS 控制，这里只给 viewBox）。
pub fn pairing_svg(payload: &str) -> Result<String, String> {
    let code = qrcodegen::QrCode::encode_text(
        payload,
        qrcodegen::QrCodeEcc::Medium,
    )
    .map_err(|error| format!("生成二维码失败：{error}"))?;
    let size = code.size() as i32;
    // 留 2 格静区；整张图缩放交给 viewBox。
    let border = 2;
    let total = size + border * 2;
    let mut modules = String::new();
    for y in 0..size {
        for x in 0..size {
            if code.get_module(x, y) {
                modules.push_str(&format!("M{} {}h1v1h-1z", x + border, y + border));
            }
        }
    }
    Ok(format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" viewBox=\"0 0 {total} {total}\" shape-rendering=\"crispEdges\" role=\"img\" aria-label=\"配对二维码\">\
         <rect width=\"{total}\" height=\"{total}\" fill=\"#ffffff\"/>\
         <path d=\"{modules}\" fill=\"#000000\"/>\
         </svg>"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn svg_contains_white_background_and_modules() {
        let svg = pairing_svg("http://192.168.1.5:15211/p/abcdef").unwrap();
        assert!(svg.starts_with("<svg"));
        assert!(svg.contains("fill=\"#ffffff\""));
        assert!(svg.contains("fill=\"#000000\""));
        assert!(svg.contains("<path d=\"M"), "应有模块路径");
    }

    #[test]
    fn rejects_payload_beyond_capacity() {
        // 超过 QR v40 容量的内容必然报错；空串反而是合法的（全空码）。
        let oversized = "a".repeat(4000);
        assert!(pairing_svg(&oversized).is_err());
    }
}
