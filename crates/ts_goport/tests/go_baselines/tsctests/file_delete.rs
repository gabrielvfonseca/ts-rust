//! Port-only test of `handle_file_delete` (Go
//! `incremental/programtosnapshot.go:170 handleFileDelete`).
//!
//! PORT: before ts#64845 Go stopped at the first gone file of a
//! random-order `SyncMap`, so Go took the global branch with chance (gone
//! global files) / (gone files), and the port took Go's more likely answer.
//! Since ts#64845 (pin 3e9236361fa9) Go visits every gone file and takes the
//! global branch when any of them affects global scope. These tests fix that
//! answer. Two test names are the names of the old answer (protected names);
//! their comments give the answer at 3e9236361fa9 and the Go runs.

use ts_goport::execute::tsc::ExitStatus;

use crate::support::child::run_command_in_child;
use crate::support::runner::TscInput;
use crate::support::test_sys::{TSC_LIB_PATH, TestSys, new_test_sys};

const PROJECT: &str = "/home/src/workspaces/project";

/// The tsconfig with `lib` set to `lib`.
fn config(lib: &str) -> String {
    format!(r#"{{"compilerOptions":{{"incremental":true,"lib":["{lib}"]}},"files":["a.ts"]}}"#)
}

/// `tsc -p tsconfig.json --pretty false` in a command child.
fn build(sys: &TestSys) -> ExitStatus {
    build_with(sys, &[])
}

/// `tsc -p tsconfig.json --pretty false` and `extra` in a command child.
fn build_with(sys: &TestSys, extra: &[&str]) -> ExitStatus {
    let mut args = ["-p", "tsconfig.json", "--pretty", "false"]
        .map(String::from)
        .to_vec();
    args.extend(extra.iter().map(|arg| arg.to_string()));
    let result = run_command_in_child(sys, &args).unwrap_or_else(|err| panic!("tsgo: {err}"));
    assert!(result.unported.is_none(), "unported {:?}", result.unported);
    result.status
}

// `lib` goes from es2016 to es2015. Two lib files leave the program:
// `lib.es2016.d.ts` (only `/// <reference lib>` lines, so it does not
// affect global scope) and then `lib.es2016.array.include.d.ts` (a global
// declaration). `lib` does not affect semantic diagnostics, so only the
// gone global file makes the program check `a.ts` again.
#[test]
fn lib_change_rechecks_when_a_later_gone_file_affects_global_scope() {
    let es2016 = format!("{TSC_LIB_PATH}/lib.es2016.d.ts");
    let include = format!("{TSC_LIB_PATH}/lib.es2016.array.include.d.ts");
    let input = TscInput {
        files: [
            (
                format!("{PROJECT}/a.ts"),
                "const x: number = fromInclude;\n".into(),
            ),
            (format!("{PROJECT}/tsconfig.json"), config("es2016").into()),
            (
                es2016,
                "/// <reference lib=\"es2015\" />\n/// <reference lib=\"es2016.array.include\" />\n"
                    .into(),
            ),
            (include, "declare const fromInclude: number;\n".into()),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    let sys = new_test_sys(&input, false);
    let fs = sys.fs_from_file_map();

    assert_eq!(build(&sys), ExitStatus::Success, "{}", sys.output_text());

    // The non-global gone file comes first in the build info, so a walk
    // that stops at the first gone file would keep the old diagnostics.
    let (build_info, ok) = fs.read_file(&format!("{PROJECT}/tsconfig.tsbuildinfo"));
    assert!(ok, "no build info");
    let start = build_info.find(r#""fileNames":["#).expect("fileNames");
    let file_names = &build_info[start..start + build_info[start..].find(']').expect("]")];
    let position = |name: &str| {
        file_names
            .find(&format!("\"{name}\""))
            .unwrap_or_else(|| panic!("{name} not in {file_names}"))
    };
    assert!(position("lib.es2016.d.ts") < position("lib.es2016.array.include.d.ts"));

    fs.write_file(&format!("{PROJECT}/tsconfig.json"), &config("es2015"))
        .expect("write tsconfig.json");
    sys.clear_output();
    let status = build(&sys);
    let output = sys.output_text();
    assert_eq!(
        status,
        ExitStatus::DiagnosticsPresentOutputsGenerated,
        "{output}"
    );
    assert!(
        output.contains("error TS2304: Cannot find name 'fromInclude'."),
        "{output}"
    );
}

// realworld3 docusaurus (incremental rebuild after a dependency change):
// three files leave the program and only `g.d.ts` affects global scope.
// Before ts#64845 Go met a module file first in 2 of 3 runs (pin
// 673a5f17d713: 134 of 200), took the branch that only rewrites the build
// info, and did not emit the unchanged `a.ts` again; the port did the same.
// Since ts#64845 one gone global file is enough: every file changes, and the
// unchanged `a.ts` is emitted again (Go 3e9236361fa9: 12 of 12 runs; Go
// fed0bf24149f: 2 of 12; bump E w2/l6 `tools/go-file-delete.sh`). The test
// keeps its old name.
#[test]
fn gone_files_mostly_modules_do_not_change_all_files() {
    let source = |name: &str| format!("{PROJECT}/src/{name}");
    let input = TscInput {
        files: [
            (
                format!("{PROJECT}/tsconfig.json"),
                r#"{"compilerOptions":{"incremental":true,"rootDir":"src","outDir":"out"},"include":["src"]}"#
                    .into(),
            ),
            (source("a.ts"), "export const a = 1;\n".into()),
            (source("g.d.ts"), "declare const g: number;\n".into()),
            (source("m1.d.ts"), "export declare const m1: number;\n".into()),
            (source("m2.d.ts"), "export declare const m2: number;\n".into()),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    let sys = new_test_sys(&input, false);
    let fs = sys.fs_from_file_map();

    let status = build_with(&sys, &["--listEmittedFiles"]);
    let output = sys.output_text();
    assert_eq!(status, ExitStatus::Success, "{output}");
    assert!(
        output.contains(&format!("TSFILE: {PROJECT}/out/a.js")),
        "{output}"
    );

    for name in ["g.d.ts", "m1.d.ts", "m2.d.ts"] {
        fs.remove(&source(name))
            .unwrap_or_else(|err| panic!("remove {name}: {err:?}"));
    }
    sys.clear_output();
    let status = build_with(&sys, &["--listEmittedFiles"]);
    let output = sys.output_text();
    assert_eq!(status, ExitStatus::Success, "{output}");
    assert!(
        output.contains(&format!("TSFILE: {PROJECT}/out/a.js")),
        "{output}"
    );
    assert!(
        output.contains(&format!("TSFILE: {PROJECT}/tsconfig.tsbuildinfo")),
        "{output}"
    );
}

// Three files leave the program and two of them, `g1.d.ts` and `g2.d.ts`,
// affect global scope. Go adds every file to the change set, so the
// unchanged `a.ts` is emitted again (before ts#64845 Go did so in 2 of 3
// runs, when it met a global file first).
#[test]
fn gone_files_mostly_global_change_all_files() {
    let source = |name: &str| format!("{PROJECT}/src/{name}");
    let input = TscInput {
        files: [
            (
                format!("{PROJECT}/tsconfig.json"),
                r#"{"compilerOptions":{"incremental":true,"rootDir":"src","outDir":"out"},"include":["src"]}"#
                    .into(),
            ),
            (source("a.ts"), "export const a = 1;\n".into()),
            (source("g1.d.ts"), "declare const g1: number;\n".into()),
            (source("g2.d.ts"), "declare const g2: number;\n".into()),
            (source("m1.d.ts"), "export declare const m1: number;\n".into()),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    let sys = new_test_sys(&input, false);
    let fs = sys.fs_from_file_map();

    let status = build_with(&sys, &["--listEmittedFiles"]);
    let output = sys.output_text();
    assert_eq!(status, ExitStatus::Success, "{output}");
    assert!(
        output.contains(&format!("TSFILE: {PROJECT}/out/a.js")),
        "{output}"
    );

    for name in ["g1.d.ts", "g2.d.ts", "m1.d.ts"] {
        fs.remove(&source(name))
            .unwrap_or_else(|err| panic!("remove {name}: {err:?}"));
    }
    sys.clear_output();
    let status = build_with(&sys, &["--listEmittedFiles"]);
    let output = sys.output_text();
    assert_eq!(status, ExitStatus::Success, "{output}");
    assert!(
        output.contains(&format!("TSFILE: {PROJECT}/out/a.js")),
        "{output}"
    );
    assert!(
        output.contains(&format!("TSFILE: {PROJECT}/tsconfig.tsbuildinfo")),
        "{output}"
    );
}

// Two files leave the program and one of them, `g.d.ts`, affects global
// scope, so the unchanged `a.ts` is emitted again. Before ts#64845 Go met
// the global file first in about half of the runs (pin 673a5f17d713,
// `followups23/tools/tie_go.sh`: 22 of 40 runs, and 105 of 200 in a later
// count), and the port took the global branch on this tie.
#[test]
fn gone_files_half_global_change_all_files() {
    let source = |name: &str| format!("{PROJECT}/src/{name}");
    let input = TscInput {
        files: [
            (
                format!("{PROJECT}/tsconfig.json"),
                r#"{"compilerOptions":{"incremental":true,"rootDir":"src","outDir":"out"},"include":["src"]}"#
                    .into(),
            ),
            (source("a.ts"), "export const a = 1;\n".into()),
            (source("g.d.ts"), "declare const g: number;\n".into()),
            (source("m.d.ts"), "export declare const m: number;\n".into()),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    let sys = new_test_sys(&input, false);
    let fs = sys.fs_from_file_map();

    let status = build_with(&sys, &["--listEmittedFiles"]);
    let output = sys.output_text();
    assert_eq!(status, ExitStatus::Success, "{output}");
    assert!(
        output.contains(&format!("TSFILE: {PROJECT}/out/a.js")),
        "{output}"
    );

    for name in ["g.d.ts", "m.d.ts"] {
        fs.remove(&source(name))
            .unwrap_or_else(|err| panic!("remove {name}: {err:?}"));
    }
    sys.clear_output();
    let status = build_with(&sys, &["--listEmittedFiles"]);
    let output = sys.output_text();
    assert_eq!(status, ExitStatus::Success, "{output}");
    assert!(
        output.contains(&format!("TSFILE: {PROJECT}/out/a.js")),
        "{output}"
    );
    assert!(
        output.contains(&format!("TSFILE: {PROJECT}/tsconfig.tsbuildinfo")),
        "{output}"
    );
}

// `lib` goes from es2022, dom, dom.iterable and dom.asynciterable to es2022,
// and `a.ts` uses `document` (the incrjs1 skeptic's `libdom.sh`). Three libs
// leave the program. Only `lib.dom.d.ts` affects global scope: at pin
// 673a5f17d713 `lib.dom.iterable.d.ts` and `lib.dom.asynciterable.d.ts`
// hold only comments (the bundled texts below). `lib` does not affect
// semantic diagnostics, so only a gone global file makes the program check
// `a.ts` again. Before ts#64845 Go gave one of two answers (pin
// 673a5f17d713), and the port took the first, as 1 gone lib of 3 is global:
// - Go meets a lib with no statements first (tsc -p 43 of 60 runs, tsc -b 30
//   of 40). Exit 0, only `tsconfig.tsbuildinfo` written.
// - Go meets `lib.dom.d.ts` first (tsc -p 17 of 60, tsc -b 10 of 40): every
//   file is checked and emitted again. `a.ts` gets TS2584 (Cannot find name
//   'document'), exit 2, and `out/a.js`, `out/b.js` and the build info are
//   written.
// Since ts#64845 the gone `lib.dom.d.ts` always gives the second answer (Go
// 3e9236361fa9 with its bundled libs: 12 of 12 runs; Go fed0bf24149f: 5 of
// 12; bump E w2/l6 `tools/go-file-delete.sh`). The test keeps its old name.
#[test]
fn lib_dom_removal_does_not_recheck_when_most_gone_libs_have_no_statements() {
    let lib = |name: &str| format!("{TSC_LIB_PATH}/{name}");
    let source = |name: &str| format!("{PROJECT}/src/{name}");
    let config = |lib: &str| {
        format!(
            r#"{{"compilerOptions":{{"incremental":true,"target":"es2022","module":"esnext","lib":[{lib}],"rootDir":"src","outDir":"out"}},"include":["src"]}}"#
        )
    };
    let input = TscInput {
        files: [
            (
                format!("{PROJECT}/tsconfig.json"),
                config(r#""es2022","dom","dom.iterable","dom.asynciterable""#).into(),
            ),
            (source("a.ts"), "export const t = document.title;\n".into()),
            (source("b.ts"), "export const b = 1;\n".into()),
            (
                lib("lib.dom.d.ts"),
                "declare var document: { title: string };\n".into(),
            ),
            (
                lib("lib.dom.iterable.d.ts"),
                include_str!("../../../libs/lib.dom.iterable.d.ts").into(),
            ),
            (
                lib("lib.dom.asynciterable.d.ts"),
                include_str!("../../../libs/lib.dom.asynciterable.d.ts").into(),
            ),
        ]
        .into_iter()
        .collect(),
        ..Default::default()
    };
    let sys = new_test_sys(&input, false);
    let fs = sys.fs_from_file_map();

    let status = build_with(&sys, &["--listEmittedFiles"]);
    let output = sys.output_text();
    assert_eq!(status, ExitStatus::Success, "{output}");
    assert!(
        output.contains(&format!("TSFILE: {PROJECT}/out/a.js")),
        "{output}"
    );

    fs.write_file(&format!("{PROJECT}/tsconfig.json"), &config(r#""es2022""#))
        .expect("write tsconfig.json");
    sys.clear_output();
    let status = build_with(&sys, &["--listEmittedFiles"]);
    let output = sys.output_text();
    assert_eq!(
        status,
        ExitStatus::DiagnosticsPresentOutputsGenerated,
        "{output}"
    );
    assert!(
        output.contains("error TS2584: Cannot find name 'document'."),
        "{output}"
    );
    for emitted in ["out/a.js", "out/b.js", "tsconfig.tsbuildinfo"] {
        assert!(
            output.contains(&format!("TSFILE: {PROJECT}/{emitted}")),
            "{output}"
        );
    }
}
