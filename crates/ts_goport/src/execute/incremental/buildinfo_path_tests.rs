//! Port of the deletion tests of execute/incremental/buildinfo_path_test.go
//! (ts#64845). `TestBuildInfoPathJSONRoundTrip` of the same Go file is in
//! build_info.rs.
//!
//! PORT: Go builds each program over a new `vfstest` map file system. Here a
//! temp dir stands for it: Go `/lib/x` and `/project/x` are `<dir>/lib/x`
//! and `<dir>/project/x`, and each program first makes the dir hold exactly
//! its files (`TestDir::set_files`), as a new map does. Go `t.Run` subtests
//! are loops; a failure names the subtest. A program is read inside
//! `enter_program` of its version, and each version is released at the end.

use super::*;
use crate::core::enter_program;
use crate::emitter::program_emit::{EmitOptions, WriteFile, WriteFileData};
use crate::execute::incremental::incremental::{create_host, parse_build_info};
use crate::execute::incremental::program::new_program;
use crate::execute::incremental::snapshot_to_build_info::snapshot_to_build_info;
use crate::frontend::compiler::{ProgramOptions, new_compiler_host};
use crate::frontend::tsoptions::{get_default_lib_file_name, new_parsed_command_line};
use crate::frontend::tspath::{ComparePathsOptions, to_path};
use crate::frontend::vfs::osvfs_fs;
use crate::program::{go_frontend_program, new_program_version, release_program};
use std::collections::BTreeMap;
use std::sync::Mutex;

// Go: execute/incremental/buildinfo_path_test.go:48 deletionTestLib
const DELETION_TEST_LIB: &str = "interface Array<T> {}
interface Boolean {}
interface CallableFunction {}
interface Function {}
interface IArguments {}
interface NewableFunction {}
interface Number {}
interface Object {}
interface RegExp {}
interface String {}
";

/// The temp dir of one test (see the module comment). Removed on drop.
struct TestDir(String);

impl TestDir {
    fn new(name: &str) -> TestDir {
        let dir = std::env::temp_dir().join(format!(
            "goport-deletion-{}-{}",
            name.replace([' ', '/'], "-"),
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("mkdir");
        TestDir(dir.to_string_lossy().replace('\\', "/"))
    }

    /// The file name of the Go path `go_path` (`/lib/...` or `/project/...`).
    fn path(&self, go_path: &str) -> String {
        format!("{}{go_path}", self.0)
    }

    /// The Go `tspath.PathKey` of `go_path`.
    fn key(&self, go_path: &str) -> Path {
        to_path(
            &self.path(go_path),
            "",
            osvfs_fs().use_case_sensitive_file_names(),
        )
    }

    /// Go `vfstest.FromMap(files)`: the dir holds exactly `files`.
    fn set_files(&self, files: &BTreeMap<String, String>) {
        for sub in ["lib", "project"] {
            let _ = std::fs::remove_dir_all(format!("{}/{sub}", self.0));
            std::fs::create_dir_all(format!("{}/{sub}", self.0)).expect("mkdir");
        }
        for (name, text) in files {
            std::fs::write(self.path(name), text).unwrap_or_else(|e| panic!("write {name}: {e}"));
        }
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Go `map[string]string{...}` of a test's files.
fn files(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
    entries
        .iter()
        .map(|(name, text)| (name.to_string(), text.to_string()))
        .collect()
}

/// An incremental program and its program version.
struct TestProgram {
    program: Program,
    version: &'static GoProgram,
}

impl TestProgram {
    /// Runs `f` with this program current.
    fn with<R>(&self, f: impl FnOnce(&Program) -> R) -> R {
        let _scope = enter_program(Some(self.version));
        f(&self.program)
    }
}

// Go: execute/incremental/buildinfo_path_test.go:60 newDeletionTestProgram
fn new_deletion_test_program(
    dir: &TestDir,
    files: &BTreeMap<String, String>,
    roots: &[&str],
    options: &CompilerOptions,
    old: Option<&TestProgram>,
) -> TestProgram {
    let mut files = files.clone();
    let lib = files["/lib/lib.d.ts"].clone();
    files.insert(format!("/lib/{}", get_default_lib_file_name(options)), lib);
    dir.set_files(&files);
    let fs = osvfs_fs();
    let use_case_sensitive_file_names = fs.use_case_sensitive_file_names();
    let project = dir.path("/project");
    let host = new_compiler_host(&project, fs, &dir.path("/lib"), None, None, None);
    let paths = roots
        .iter()
        .map(|name| format!("{project}/{name}"))
        .collect();
    let config = Rc::new(new_parsed_command_line(
        Rc::new(options.clone()),
        paths,
        None,
        ComparePathsOptions {
            use_case_sensitive_file_names,
            current_directory: project,
        },
    ));
    let np = {
        let _scope = enter_program(None);
        Rc::new(crate::frontend::compiler::new_program(ProgramOptions {
            host,
            config,
            use_source_of_project_reference: false,
            single_threaded: Tristate::Unknown,
            typings_location: String::new(),
            project_name: String::new(),
            create_module_resolver: None,
            skip_module_resolution: false,
        }))
    };
    let version = new_program_version(&np, None);
    let _scope = enter_program(Some(version));
    let host = create_host(go_frontend_program().expect("frontend").host().clone());
    TestProgram {
        program: new_program(old.map(|old| &old.program), host, None, false),
        version,
    }
}

/// Go `&Program{snapshot: &snapshot{}}`: a program with an empty snapshot
/// and the file infos `infos`. Call it with a program current.
fn snapshot_only_program(infos: impl IntoIterator<Item = (Path, FileInfo)>) -> Program {
    let host = create_host(go_frontend_program().expect("frontend").host().clone());
    let program = new_program(None, host, None, false);
    let mut snapshot = Snapshot::new(options());
    snapshot.file_infos.extend(infos);
    *program.snapshot.borrow_mut() = snapshot;
    program
}

/// Go `&toProgramSnapshot{program: current.program, snapshot: current.snapshot,
/// oldProgram: old}`. It takes `current`'s snapshot, which the caller reads
/// in the result. Call it with `current` current.
fn to_program_snapshot<'a>(current: &TestProgram, old: &'a Program) -> ToProgramSnapshot<'a> {
    let snapshot = std::mem::replace(
        &mut *current.program.snapshot.borrow_mut(),
        Snapshot::new(options()),
    );
    ToProgramSnapshot {
        program: current.version,
        old_program: Some(old),
        snapshot,
        global_file_removed: false,
    }
}

/// Go `GetSemanticDiagnostics(ctx, nil)`.
fn semantic_diagnostics(program: &TestProgram) -> Vec<Diagnostic> {
    program.with(|program| program.get_semantic_diagnostics(Node::NIL))
}

/// Go `Emit(ctx, compiler.EmitOptions{})`.
fn emit(program: &TestProgram) {
    program.with(|program| {
        let _ = program.emit(EmitOptions::default());
    });
}

fn release(programs: impl IntoIterator<Item = TestProgram>) {
    for program in programs {
        let version = program.version;
        drop(program);
        release_program(version);
    }
}

// Go: execute/incremental/buildinfo_path_test.go:72 TestDeletedGlobalInvalidatesAllSourceFiles
#[test]
fn test_deleted_global_invalidates_all_source_files() {
    let dir = TestDir::new("deleted-global");
    let options = CompilerOptions {
        incremental: Tristate::True,
        no_emit: Tristate::True,
        ..CompilerOptions::default()
    };
    let current = new_deletion_test_program(
        &dir,
        &files(&[
            ("/lib/lib.d.ts", DELETION_TEST_LIB),
            ("/project/consumer.ts", "export const unchanged = 1;"),
        ]),
        &["consumer.ts"],
        &options,
        None,
    );
    let _scope = enter_program(Some(current.version));
    let infos: Vec<(Path, FileInfo)> = current
        .program
        .snapshot
        .borrow()
        .file_infos
        .iter()
        .map(|(path, info)| (path.clone(), info.clone()))
        .chain(
            ["/project/first.d.ts", "/project/second.d.ts"]
                .map(|name| (dir.key(name), FileInfo::default())),
        )
        .collect();
    let old = snapshot_only_program(infos);
    let deleted_order = || -> Vec<Path> {
        let new_infos = &current.program.snapshot.borrow().file_infos;
        old.snapshot
            .borrow()
            .file_infos
            .keys()
            .filter(|path| !new_infos.contains_key(*path))
            .cloned()
            .collect()
    };
    let order = deleted_order();
    assert_eq!(
        order,
        deleted_order(),
        "file iteration changed without writes"
    );
    old.snapshot
        .borrow_mut()
        .file_infos
        .get_mut(order.last().expect("deleted files"))
        .expect("an old file info")
        .affects_global_scope = true;
    let mut to = to_program_snapshot(&current, &old);
    to.handle_file_delete();
    let consumer = dir.key("/project/consumer.ts");
    assert!(
        to.global_file_removed && to.snapshot.changed_files_set.contains(&consumer),
        "global deletion after non-global deletion did not invalidate consumer: global={} changed={}",
        to.global_file_removed,
        to.snapshot.changed_files_set.contains(&consumer)
    );
    drop(to);
    drop(old);
    drop(_scope);
    release([current]);
}

// Go: execute/incremental/buildinfo_path_test.go:110 TestGlobalDeletionInvalidatesRetainedLibraryDiagnostics
#[test]
fn test_global_deletion_invalidates_retained_library_diagnostics() {
    for single in [Tristate::False, Tristate::True] {
        for kind in ["new error", "cleared error", "scope loss"] {
            let name = format!("{single:?}/{kind}");
            let dir = TestDir::new(&format!("retained-lib-{name}"));
            let options = CompilerOptions {
                incremental: Tristate::True,
                no_emit: Tristate::True,
                single_threaded: single,
                ts_build_info_file: dir.path("/project/state.tsbuildinfo"),
                ..CompilerOptions::default()
            };
            let mut library = format!(
                "{DELETION_TEST_LIB}interface LibraryConsumer {{ property: RemovedGlobal; }}"
            );
            let mut global = "interface RemovedGlobal { x: string; }".to_string();
            if kind == "cleared error" {
                library = format!("{DELETION_TEST_LIB}interface Shared<T> {{}}");
                global = "interface Shared<T, U> {}".to_string();
            }
            let mut files = files(&[
                ("/lib/lib.d.ts", library.as_str()),
                ("/project/global.d.ts", global.as_str()),
                ("/project/consumer.ts", "export const unchanged = 1;"),
                ("/project/module.d.ts", "export interface Unrelated {}"),
            ]);
            let old = new_deletion_test_program(
                &dir,
                &files,
                &["global.d.ts", "consumer.ts", "module.d.ts"],
                &options,
                None,
            );
            semantic_diagnostics(&old);
            emit(&old);
            let mut roots = vec!["consumer.ts"];
            if kind == "scope loss" {
                files.insert("/project/global.d.ts".into(), format!("export {global}"));
                roots.push("global.d.ts");
            } else {
                files.remove("/project/global.d.ts");
            }
            files.remove("/project/module.d.ts");
            let current = new_deletion_test_program(&dir, &files, &roots, &options, Some(&old));
            let cold = new_deletion_test_program(&dir, &files, &roots, &options, None);
            let actual: Vec<i32> = semantic_diagnostics(&current)
                .iter()
                .map(Diagnostic::code)
                .collect();
            let expected: Vec<i32> = semantic_diagnostics(&cold)
                .iter()
                .map(Diagnostic::code)
                .collect();
            if kind == "cleared error" {
                assert!(
                    expected.is_empty(),
                    "{name}: cold build should have no errors: {expected:?}"
                );
            } else {
                assert_eq!(
                    expected,
                    [2304],
                    "{name}: cold build should report one TS2304"
                );
            }
            assert_eq!(
                actual.len(),
                expected.len(),
                "{name}: incremental diagnostics={}, cold={}",
                actual.len(),
                expected.len()
            );
            if !actual.is_empty() {
                assert_eq!(
                    actual[0], expected[0],
                    "{name}: incremental diagnostic={}, cold={}",
                    actual[0], expected[0]
                );
            }
            release([current, cold, old]);
        }
    }
}

// Go: execute/incremental/buildinfo_path_test.go:165 TestEmptyProgramDeletionEmitsBuildInfo
#[test]
fn test_empty_program_deletion_emits_build_info() {
    let tristate = |on: bool| if on { Tristate::True } else { Tristate::False };
    for flags in 0..8 {
        for roots in [
            &["global.d.ts"][..],
            &["module.d.ts"][..],
            &["global.d.ts", "module.d.ts"][..],
        ] {
            let name = format!("{flags}/{roots:?}");
            let dir = TestDir::new(&format!("empty-{flags}-{}", roots.join("-")));
            let options = CompilerOptions {
                incremental: Tristate::True,
                ts_build_info_file: dir.path("/project/state.tsbuildinfo"),
                no_check: tristate(flags & 1 != 0),
                no_emit: tristate(flags & 2 != 0),
                composite: tristate(flags & 4 != 0),
                ..CompilerOptions::default()
            };
            let mut files = files(&[
                ("/lib/lib.d.ts", DELETION_TEST_LIB),
                ("/project/global.d.ts", "interface RemovedGlobal {}"),
                ("/project/module.d.ts", "export interface Unrelated {}"),
            ]);
            let old = new_deletion_test_program(&dir, &files, roots, &options, None);
            semantic_diagnostics(&old);
            emit(&old);
            assert!(
                !old.program.snapshot.borrow().build_info_emit_pending,
                "{name}: initial build info was not emitted"
            );
            files.remove("/project/global.d.ts");
            files.remove("/project/module.d.ts");
            let current = new_deletion_test_program(&dir, &files, &[], &options, Some(&old));
            assert!(
                current.with(|program| program.get_source_files().is_empty()),
                "{name}: new program is not empty"
            );
            let build_infos = Arc::new(Mutex::new(Vec::new()));
            let write_file: WriteFile = Arc::new({
                let build_infos = build_infos.clone();
                move |file_name: &str, text: &str, _data: &mut WriteFileData| {
                    if file_name.ends_with(".tsbuildinfo") {
                        build_infos
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .push(text.to_string());
                    }
                    Ok(())
                }
            });
            current.with(|program| {
                let _ = program.emit(EmitOptions {
                    write_file: Some(write_file),
                    ..EmitOptions::default()
                });
            });
            let build_infos = build_infos
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            for text in &build_infos {
                let info =
                    parse_build_info(text).unwrap_or_else(|| panic!("{name}: cannot parse {text}"));
                let empty = info.file_names.as_ref().is_none_or(Vec::is_empty)
                    && info.file_infos.as_ref().is_none_or(Vec::is_empty)
                    && info.change_file_set.as_ref().is_none_or(Vec::is_empty)
                    && info
                        .affected_files_pending_emit
                        .as_ref()
                        .is_none_or(Vec::is_empty);
                assert!(
                    empty,
                    "{name}: empty program persisted removed state: {text}"
                );
            }
            assert_eq!(
                build_infos.len(),
                1,
                "{name}: build-info writes after deleting every source"
            );
            release([current, old]);
        }
    }
}

// Go: execute/incremental/buildinfo_path_test.go:223 TestUncheckedDeletedFilesDiscardPendingState
#[test]
fn test_unchecked_deleted_files_discard_pending_state() {
    let dir = TestDir::new("unchecked-deleted");
    let options = CompilerOptions {
        incremental: Tristate::True,
        no_emit: Tristate::True,
        no_check: Tristate::True,
        ts_build_info_file: dir.path("/project/state.tsbuildinfo"),
        ..CompilerOptions::default()
    };
    let mut files = files(&[
        ("/lib/lib.d.ts", DELETION_TEST_LIB),
        ("/project/stable.ts", "export const stable = 1;"),
    ]);
    let old = new_deletion_test_program(&dir, &files, &["stable.ts"], &options, None);
    emit(&old);
    files.insert("/project/first.ts".into(), "export const first = 1;".into());
    files.insert(
        "/project/second.ts".into(),
        "export const second = 1;".into(),
    );
    let added = new_deletion_test_program(
        &dir,
        &files,
        &["stable.ts", "first.ts", "second.ts"],
        &options,
        Some(&old),
    );
    emit(&added);
    let paths = ["/project/first.ts", "/project/second.ts"].map(|name| dir.key(name));
    for path in &paths {
        let mut snapshot = added.program.snapshot.borrow_mut();
        assert!(
            snapshot.changed_files_set.contains(path),
            "new unchecked file {path:?} not pending"
        );
        snapshot
            .affected_files_pending_emit
            .insert(path.clone(), FileEmitKind::JS);
    }
    files.remove("/project/first.ts");
    files.remove("/project/second.ts");
    let current = new_deletion_test_program(&dir, &files, &["stable.ts"], &options, Some(&added));
    for path in &paths {
        let snapshot = current.program.snapshot.borrow();
        assert!(
            !snapshot.changed_files_set.contains(path),
            "removed unchecked file {path:?} still in change set"
        );
        assert!(
            !snapshot.affected_files_pending_emit.contains_key(path),
            "removed file {path:?} still pending emit"
        );
    }
    let info = current
        .with(|program| {
            snapshot_to_build_info(&program.snapshot.borrow(), &options.ts_build_info_file)
        })
        .expect("snapshotToBuildInfo");
    let names = info.file_names.as_ref().map_or(0, Vec::len);
    let infos = info.file_infos.as_ref().map_or(0, Vec::len);
    let changes = info.change_file_set.as_ref().map_or(0, Vec::len);
    assert!(
        names == infos && changes == 0,
        "invalid build-info file state: names={names} infos={infos} changes={changes}"
    );
    release([current, added, old]);
}

// Go: execute/incremental/buildinfo_path_test.go:265 TestModuleDeletionPreservesUnrelatedCachedDiagnostics
#[test]
fn test_module_deletion_preserves_unrelated_cached_diagnostics() {
    for skip in [Tristate::False, Tristate::True] {
        let dir = TestDir::new(&format!("module-deletion-{skip:?}"));
        let options = CompilerOptions {
            incremental: Tristate::True,
            no_emit: Tristate::True,
            skip_default_lib_check: skip,
            ts_build_info_file: dir.path("/project/state.tsbuildinfo"),
            ..CompilerOptions::default()
        };
        let mut files = files(&[
            ("/lib/lib.d.ts", DELETION_TEST_LIB),
            ("/project/consumer.ts", "export const unchanged = 1;"),
            ("/project/module.d.ts", "export interface Unrelated {}"),
        ]);
        let old = new_deletion_test_program(
            &dir,
            &files,
            &["consumer.ts", "module.d.ts"],
            &options,
            None,
        );
        semantic_diagnostics(&old);
        emit(&old);
        files.remove("/project/module.d.ts");
        let current =
            new_deletion_test_program(&dir, &files, &["consumer.ts"], &options, Some(&old));
        assert!(
            !current
                .program
                .snapshot
                .borrow()
                .changed_files_set
                .contains(&dir.key("/project/consumer.ts")),
            "{skip:?}: unrelated module deletion invalidated consumer"
        );
        let files = current.with(Program::get_source_files);
        for file in files {
            let path = current.with(|_| Path(source_file_info(file).path.clone()));
            let previous = old
                .program
                .snapshot
                .borrow()
                .semantic_diagnostics_per_file
                .get(&path)
                .map(|diagnostics| diagnostics.id);
            assert!(
                previous.is_some(),
                "{skip:?}: initial diagnostics for {path:?} were not cached"
            );
            let cached = current
                .program
                .snapshot
                .borrow()
                .semantic_diagnostics_per_file
                .get(&path)
                .map(|diagnostics| diagnostics.id);
            assert_eq!(
                cached, previous,
                "{skip:?}: unrelated cached diagnostics for {path:?} not reused"
            );
        }
        release([current, old]);
    }
}

// Go: execute/incremental/buildinfo_path_test.go:301 TestDeletedFilesCleanupAfterGlobalDeletion
#[test]
fn test_deleted_files_cleanup_after_global_deletion() {
    let dir = TestDir::new("cleanup-global");
    let options = CompilerOptions {
        incremental: Tristate::True,
        ..CompilerOptions::default()
    };
    let current = new_deletion_test_program(
        &dir,
        &files(&[("/lib/lib.d.ts", DELETION_TEST_LIB)]),
        &[],
        &options,
        None,
    );
    let _scope = enter_program(Some(current.version));
    let deleted = [
        "/project/first.d.ts",
        "/project/second.d.ts",
        "/project/third.d.ts",
    ]
    .map(|name| dir.key(name));
    let old = snapshot_only_program(deleted.iter().map(|path| {
        let info = FileInfo {
            affects_global_scope: true,
            ..FileInfo::default()
        };
        (path.clone(), info)
    }));
    {
        let mut snapshot = current.program.snapshot.borrow_mut();
        for path in &deleted {
            snapshot.changed_files_set.insert(path.clone());
            snapshot
                .affected_files_pending_emit
                .insert(path.clone(), FileEmitKind::JS);
        }
    }
    let mut to = to_program_snapshot(&current, &old);
    to.handle_file_delete();
    for path in &deleted {
        assert!(
            !to.snapshot.changed_files_set.contains(path),
            "removed path {path:?} still in change set"
        );
        assert!(
            !to.snapshot.affected_files_pending_emit.contains_key(path),
            "removed path {path:?} still pending emit"
        );
    }
    assert!(
        to.snapshot.build_info_emit_pending,
        "global deletion did not mark build info pending"
    );
    drop(to);
    drop(old);
    drop(_scope);
    release([current]);
}

// Go: execute/incremental/buildinfo_path_test.go:327 TestRemovedPendingPathsWithoutFileInfo
#[test]
fn test_removed_pending_paths_without_file_info() {
    let dir = TestDir::new("removed-pending");
    let options = CompilerOptions {
        incremental: Tristate::True,
        no_check: Tristate::True,
        no_emit: Tristate::True,
        ts_build_info_file: dir.path("/project/state.tsbuildinfo"),
        ..CompilerOptions::default()
    };
    let files = files(&[
        ("/lib/lib.d.ts", DELETION_TEST_LIB),
        ("/project/stable.ts", "export const stable = 1;"),
    ]);
    let old = new_deletion_test_program(&dir, &files, &["stable.ts"], &options, None);
    emit(&old);
    let missing = dir.key("/project/missing.ts");
    {
        let mut snapshot = old.program.snapshot.borrow_mut();
        snapshot.changed_files_set.insert(missing.clone());
        snapshot
            .affected_files_pending_emit
            .insert(missing.clone(), FileEmitKind::JS);
    }
    let current = new_deletion_test_program(&dir, &files, &["stable.ts"], &options, Some(&old));
    {
        let snapshot = current.program.snapshot.borrow();
        assert!(
            !snapshot.changed_files_set.contains(&missing),
            "removed pending path without file info was retained"
        );
        assert!(
            !snapshot.affected_files_pending_emit.contains_key(&missing),
            "removed pending emit path without file info was retained"
        );
        assert!(
            snapshot.build_info_emit_pending,
            "discarding obsolete pending state did not mark build info pending"
        );
    }
    let info = current
        .with(|program| {
            snapshot_to_build_info(&program.snapshot.borrow(), &options.ts_build_info_file)
        })
        .expect("snapshotToBuildInfo");
    let names = info.file_names.as_ref().map_or(0, Vec::len);
    let infos = info.file_infos.as_ref().map_or(0, Vec::len);
    assert_eq!(names, infos, "invalid cached file state");
    release([current, old]);
}

// Go: execute/incremental/buildinfo_path_test.go:360 TestForEachDeletedFileVisitsEveryRemovedPath
#[test]
fn test_for_each_deleted_file_visits_every_removed_path() {
    fn visit_orders(order: &mut [Path; 4], index: usize) {
        if index != order.len() {
            for next in index..order.len() {
                order.swap(index, next);
                visit_orders(order, index + 1);
                order.swap(index, next);
            }
            return;
        }
        for retained in 0..16 {
            for global in 0..16 {
                let mut current: FxIndexMap<Path, FileInfo> = FxIndexMap::default();
                let mut infos: FxHashMap<Path, FileInfo> = FxHashMap::default();
                let mut expected = Vec::new();
                for (i, path) in order.iter().enumerate() {
                    infos.insert(
                        path.clone(),
                        FileInfo {
                            affects_global_scope: global & (1 << i) != 0,
                            ..FileInfo::default()
                        },
                    );
                    if retained & (1 << i) != 0 {
                        current.insert(path.clone(), FileInfo::default());
                    } else {
                        expected.push(path.clone());
                    }
                }
                current.insert(Path("/new.ts".into()), FileInfo::default());
                let mut actual = Vec::new();
                for_each_deleted_file(
                    |callback| {
                        for path in order.iter() {
                            assert!(
                                callback(path, &infos[path]),
                                "deleted-file enumeration stopped early"
                            );
                        }
                    },
                    &current,
                    |path, info| {
                        assert!(
                            std::ptr::eq(info, &infos[path]),
                            "deleted-file callback lost old metadata"
                        );
                        actual.push(path.clone());
                    },
                );
                assert_eq!(
                    actual, expected,
                    "order={order:?} retained={retained} global={global}"
                );
            }
        }
    }
    let mut order = ["/first.ts", "/second.ts", "/third.ts", "/fourth.ts"].map(|p| Path(p.into()));
    visit_orders(&mut order, 0);
}

// Go: execute/incremental/buildinfo_path_test.go:409 TestGlobalDeletionPreservesSkippedLibraryDiagnostics
#[test]
fn test_global_deletion_preserves_skipped_library_diagnostics() {
    for skip_default in [false, true] {
        let dir = TestDir::new(&format!("skipped-lib-{skip_default}"));
        let mut options = CompilerOptions {
            incremental: Tristate::True,
            no_emit: Tristate::True,
            ..CompilerOptions::default()
        };
        if skip_default {
            options.skip_default_lib_check = Tristate::True;
        } else {
            options.skip_lib_check = Tristate::True;
        }
        let library =
            format!("{DELETION_TEST_LIB}interface LibraryConsumer {{ property: RemovedGlobal; }}");
        let mut files = files(&[
            ("/lib/lib.d.ts", library.as_str()),
            ("/project/global.d.ts", "interface RemovedGlobal {}"),
            ("/project/consumer.ts", "export const unchanged = 1;"),
        ]);
        let old = new_deletion_test_program(
            &dir,
            &files,
            &["consumer.ts", "global.d.ts"],
            &options,
            None,
        );
        semantic_diagnostics(&old);
        emit(&old);
        files.remove("/project/global.d.ts");
        let current =
            new_deletion_test_program(&dir, &files, &["consumer.ts"], &options, Some(&old));
        let libraries: Vec<Path> = current.with(|program| {
            program
                .get_source_files()
                .into_iter()
                .map(|file| Path(source_file_info(file).path.clone()))
                .filter(|path| program.is_source_file_default_library(path))
                .collect()
        });
        for path in &libraries {
            let previous = old
                .program
                .snapshot
                .borrow()
                .semantic_diagnostics_per_file
                .get(path)
                .map(|diagnostics| diagnostics.id);
            let cached = current
                .program
                .snapshot
                .borrow()
                .semantic_diagnostics_per_file
                .get(path)
                .map(|diagnostics| diagnostics.id);
            assert!(
                previous.is_some() && cached == previous,
                "{skip_default}: skipped library diagnostics for {path:?} not reused"
            );
            assert!(
                !current
                    .program
                    .snapshot
                    .borrow()
                    .changed_files_set
                    .contains(path),
                "{skip_default}: global removal marked skipped library {path:?} changed"
            );
        }
        let diagnostics = semantic_diagnostics(&current);
        assert!(
            diagnostics.is_empty(),
            "{skip_default}: skipped library produced {} diagnostics",
            diagnostics.len()
        );
        release([current, old]);
    }
}
