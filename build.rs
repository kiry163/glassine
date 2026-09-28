fn main() {
    #[cfg(windows)]
    {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/glassine.ico");
        res.set_manifest_file("assets/glassine.manifest");
        res.compile().expect("embedding the icon and manifest");
    }
}
