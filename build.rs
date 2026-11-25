//! Embeds the app icon and the version information in the Windows executable.
//! Explorer, the Start menu, shortcuts and Task Manager show them (Task Manager
//! lists the processes by FileDescription).

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");

    #[cfg(windows)]
    {
        let mut resource = winresource::WindowsResource::new();
        resource.set_icon("assets/icon.ico");
        resource.set("FileDescription", "Simple RTSP Viewer");
        resource.set("ProductName", "Simple RTSP Viewer");
        resource.set("CompanyName", "Matheus Nascimento");
        resource.set("InternalName", "Simple RTSP Viewer");
        resource.set("OriginalFilename", "Simple RTSP Viewer.exe");
        resource.set(
            "LegalCopyright",
            "Copyright (c) 2026 Matheus Nascimento. MIT License.",
        );
        if let Err(err) = resource.compile() {
            panic!("failed to embed the Windows resources: {err}");
        }
    }
}
