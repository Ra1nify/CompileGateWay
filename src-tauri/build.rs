fn main() {
    // Tauri 的构建步骤只在带 gui feature 时需要。
    // headless daemon(`--no-default-features --bin compilegateway-daemon`)不碰它,
    // 所以没有 GTK / WebKit 开发包的小主机也能编。
    #[cfg(feature = "gui")]
    tauri_build::build();
}
