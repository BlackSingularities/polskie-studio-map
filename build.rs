//! Zasoby Windows: ikona i metadane pliku wykonywalnego.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/icon.ico");
        res.set("ProductName", "Polskie Studio Map");
        res.set("FileDescription", "Polskie Studio Map — atlas warstw mapowych Polski 3D");
        res.set("CompanyName", "BlackSingularities");
        res.set("LegalCopyright", "© 2026 Adam Rędzikowski");
        if let Err(e) = res.compile() {
            println!("cargo:warning=Nie można dołączyć zasobów Windows: {e}");
        }
    }
}
