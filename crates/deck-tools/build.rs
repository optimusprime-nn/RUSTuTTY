//! Gives each tool RUSTuTTY's icon and the details Windows shows under
//! Properties > Details, so none of them shows as a blank program.

const TOOLS: [(&str, &str); 5] = [
    ("plink", "Plink: command line SSH, Telnet and Rlogin client"),
    ("pscp", "PSCP: command line file copy over SFTP or SCP"),
    ("psftp", "PSFTP: interactive SFTP client"),
    ("puttygen", "PuTTYgen: SSH key generator and converter"),
    ("pageant", "Pageant: SSH authentication agent"),
];

fn main() {
    let version = std::env::var("CARGO_PKG_VERSION").unwrap();
    // 1.0.0-beta.5 is file version 1,0,0,0: the parts Windows can hold
    let numbers: Vec<&str> = version.split(['.', '-']).take(3).collect();
    let numeric = format!("{},0", numbers.join(","));
    let manifest_dir = std::path::PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let icon = manifest_dir.join("../rustutty/assets/deck.ico").canonicalize().unwrap();
    let icon = icon.display().to_string().trim_start_matches(r"\\?\").replace('\\', "/");
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    println!("cargo:rerun-if-changed=../rustutty/assets/deck.ico");
    for (name, description) in TOOLS {
        let rc = out.join(format!("{name}.rc"));
        std::fs::write(
            &rc,
            format!(
                r#"1 ICON "{icon}"
1 VERSIONINFO
FILEVERSION {numeric}
PRODUCTVERSION {numeric}
FILEOS 0x40004
FILETYPE 0x1
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "080904B0"
    BEGIN
      VALUE "CompanyName", "RUSTuTTY"
      VALUE "FileDescription", "{description}"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "{name}"
      VALUE "OriginalFilename", "{name}.exe"
      VALUE "ProductName", "RUSTuTTY"
      VALUE "ProductVersion", "{version}"
      VALUE "LegalCopyright", "MIT licence. Follows PuTTY, copyright Simon Tatham and contributors"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x809, 1200
  END
END
"#
            ),
        )
        .unwrap();
        embed_resource::compile_for(&rc, [name], embed_resource::NONE).manifest_optional().unwrap();
    }
}
