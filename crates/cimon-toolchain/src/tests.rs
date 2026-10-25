use super::*;

#[test]
fn extract_refuses_zip_slip() {
    use std::io::Write;
    let d = tempfile::tempdir().unwrap();
    let z = d.path().join("evil.zip");
    {
        let f = std::fs::File::create(&z).unwrap();
        let mut w = zip::ZipWriter::new(f);
        let o = zip::write::SimpleFileOptions::default();
        w.start_file("../escape.txt", o).unwrap();
        w.write_all(b"x").unwrap();
        w.finish().unwrap();
    }
    let out = d.path().join("out");
    std::fs::create_dir_all(&out).unwrap();
    let e = extract(&z, &out).unwrap_err();
    assert!(e.message.contains("escapes"), "{}", e.message);
    assert!(!d.path().join("escape.txt").exists());
}

#[test]
fn version_key_separates_package_sets() {
    let def: KindDef = toml::from_str("tier = \"managed\"\ninstaller = \"uv-python\"\n").unwrap();
    let a = ToolchainReq { packages: Some(vec!["pyyaml==6.0.3".into()]), ..Default::default() };
    let b = ToolchainReq { packages: Some(vec!["pyyaml==6.0.2".into()]), ..Default::default() };
    assert_ne!(version_key(&def, "3.13.14", &a), version_key(&def, "3.13.14", &b));
    assert_eq!(version_key(&def, "3.13.14", &ToolchainReq::default()), "3.13.14");
}

#[test]
fn fill_placeholders() {
    assert_eq!(fill("jdk-{version_url}", "17.0.19+10"), "jdk-17.0.19%2B10");
    assert_eq!(fill("node-v{version}-win", "24.1.0"), "node-v24.1.0-win");
}
