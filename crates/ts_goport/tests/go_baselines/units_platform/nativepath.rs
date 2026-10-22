//! Go: `internal/nativepath/network_test.go` (ts#64832).
//! `symlink_windows_test.go` is not ported (see the module comment).

use ts_goport::frontend::nativepath::is_network_path;

// Go: network_test.go:9 TestIsNetworkPath
#[test]
fn test_is_network_path() {
    for path in [
        "//server",
        "//server/share/file.ts",
        "\\\\server\\share\\file.ts",
        "/\\server/share",
        "\\/server/share",
        "//?/UNC/server/share",
        "\\\\?\\UNC\\server\\share",
        "//./UNC/server/share",
        "\\\\.\\GLOBALROOT\\Device\\Mup\\server\\share",
        "\\\\?\\c:\\file.ts",
        "\\\\wsl$\\Ubuntu\\home",
        "//wsl.localhost/Ubuntu/home",
        "/??",
        "/??/UNC/server/share",
        "\\??\\UNC\\server\\share",
        "\\??\\GLOBALROOT\\Device\\Mup\\server\\share",
    ] {
        assert!(is_network_path(path), "{path}");
    }
    for path in [
        "",
        "/",
        "\\",
        "a",
        "a//b",
        "./a",
        "../a",
        "/a",
        "/a//b",
        "/?",
        "/???",
        "/??a",
        "/a/??/b",
        "c:",
        "c:/",
        "c:\\file.ts",
        "c:file.ts",
        "file:///c:/file.ts",
        "file://server/share",
        "^/untitled/ts-nul-authority/Untitled-1",
        "bundled:///libs/lib.d.ts",
    ] {
        assert!(!is_network_path(path), "{path}");
    }
}
