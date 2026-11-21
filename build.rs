//! Embeds the app icon in the Windows executable, so Explorer, the Start
//! menu and shortcuts show it.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");

    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/icon.ico");
        if let Err(err) = resource.compile() {
            panic!("failed to embed the app icon: {err}");
        }
    }
}
