//! End-to-end tests: bundle each fixture in `tests/solutions`, then compile and
//! run the bundled code and check what it actually does.
//!
//! These replace the previous md5 snapshot tests. Hashing the output only asserted
//! "byte-identical to the last recorded run", which broke on every dependency bump
//! and every cosmetic change to the bundler, and told us nothing about *what*
//! changed when it did break. What we actually care about is that the single `.rs`
//! file we hand to a judge compiles and behaves like the original crate did.
//!
//! Note that `cargo_equip::run` already `cargo check`s its own output, so reaching
//! the end of `bundle` is itself an assertion. Compiling and running on top of that
//! additionally covers linking and runtime behavior.

use cargo_equip::shell::Shell;
use clap::Parser as _;
use once_cell::sync::Lazy;
use std::{
    cell::RefCell,
    env, fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    rc::Rc,
    str,
    sync::{Mutex, MutexGuard},
};

/// `name: <stdin> => <expected stdout>;`
///
/// The fixtures are in `tests/solutions/src/bin`. Test names use `_` where the bin
/// name uses `-`.
macro_rules! bundle_tests {
    ($($name:ident: $stdin:expr => $expected_stdout:expr;)*) => {
        $(
            #[test]
            fn $name() -> anyhow::Result<()> {
                // workaround for grcov
                env::remove_var("RUSTFLAGS");

                let name = stringify!($name).replace('_', "-");
                let bundled = bundle(&name, LOCK.lock().unwrap())?;
                compile_and_run(&name, &bundled, $stdin, $expected_stdout)
            }
        )*
    };
}

static LOCK: Lazy<Mutex<()>> = Lazy::new(|| Mutex::new(()));

bundle_tests! {
    fixedbitset: "" => "";
    lazy_static_with_macro_use: "" => "";
    lazy_static_with_use: "" => "";
    maplit_with_macro_use: "" => "";
    maplit_with_use: "" => "";
    memoise_with_macro_use: "" => FIB;
    memoise_with_use: "" => FIB;
    multimap_with_macro_use: "" => "";
    multimap_with_use: "" => "";
    permutohedron: "" => "";
    proconio_with_macro_use: "hello\n" => "hello\n";
    proconio_with_use: "hello\n" => "hello\n";
    rustc_hash: "" => "";
    smallvec_with_macro_use: "" => "";
    smallvec_with_use: "" => "";
    strsim: "" => "";
    whiteread: "42\n" => "";
}

/// `fib(0)..=fib(10)`, one per line, as printed by the `memoise` fixtures.
const FIB: &str = "0\n1\n1\n2\n3\n5\n8\n13\n21\n34\n55\n";

#[test]
fn stacked_attributes_preserve_only_original_invocations() -> anyhow::Result<()> {
    let name = "stacked-attributes";
    let bundled = bundle(name, LOCK.lock().unwrap())?;
    let original = include_str!("solutions/src/bin/stacked-attributes.rs");
    let main = original
        .split_once("#[preserve_first]")
        .unwrap()
        .1
        .split_once("\n\nconst ORIGINAL")
        .unwrap()
        .0;
    assert!(
        bundled.contains(&format!("/*#[preserve_first]{main}*/")),
        "{}",
        bundled
    );
    assert_eq!(bundled.matches("preserve_second]").count(), 1, "{bundled}");
    assert_eq!(
        bundled.matches("/*expand_number!()*/").count(),
        1,
        "{bundled}"
    );
    compile_and_run(name, &bundled, "", "84\n")
}

/// Runs `cargo equip` over the fixture and returns the bundled code.
fn bundle(name: &str, _: MutexGuard<'_, ()>) -> anyhow::Result<String> {
    let stdout = Rc::new(RefCell::default());

    cargo_equip::run(
        cargo_equip::Opt::try_parse_from([
            "",
            "equip",
            "--toolchain",
            &env::var("CARGO_EQUIP_TEST_NIGHTLY_TOOLCHAIN")
                .unwrap_or_else(|_| "nightly".to_owned()),
            "--remove",
            "docs",
            "--minify",
            "libs",
            "--bin",
            name,
        ])?,
        cargo_equip::Context {
            cwd: manifest_dir().join("tests").join("solutions"),
            cache_dir: manifest_dir().join("tests").join("cache"),
            shell: &mut Shell::from_stdout(Box::new(Writer(stdout.clone()))),
        },
    )?;

    let bundled = String::from_utf8(Rc::try_unwrap(stdout).unwrap().into_inner())?;

    // Set `CARGO_EQUIP_DUMP_DIR` to dump the bundled code somewhere. Useful when
    // refactoring: capture a baseline, make the change, and `diff -r` the two
    // directories to see exactly what moved.
    if let Some(dir) = env::var_os("CARGO_EQUIP_DUMP_DIR") {
        let dir = Path::new(&dir);
        fs::create_dir_all(dir)?;
        fs::write(dir.join(format!("{}.rs", name)), &bundled)?;
    }

    return Ok(bundled);

    struct Writer(Rc<RefCell<Vec<u8>>>);

    impl Write for Writer {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.borrow_mut().write(buf)
        }

        fn flush(&mut self) -> io::Result<()> {
            self.0.borrow_mut().flush()
        }
    }
}

/// Compiles the bundled code as a standalone `.rs` file with no `extern` crates
/// available, which is what a judge does, then runs it.
fn compile_and_run(
    name: &str,
    bundled: &str,
    stdin: &str,
    expected_stdout: &str,
) -> anyhow::Result<()> {
    let dir = tempfile::Builder::new()
        .prefix("cargo-equip-test")
        .tempdir()?;
    let src = dir.path().join(format!("{}.rs", name));
    let exe = dir.path().join(name);
    fs::write(&src, bundled)?;

    // The fixtures are `edition = "2018"`; see `tests/solutions/Cargo.toml`.
    // Lints are off: we are compiling third-party code we do not control, and this
    // test is about whether it builds and runs, not whether it is lint-clean.
    let rustc = Command::new("rustc")
        .args([
            "--edition",
            "2018",
            "--crate-type",
            "bin",
            "-A",
            "warnings",
            "-o",
        ])
        .args([&exe, &src])
        .output()?;
    anyhow::ensure!(
        rustc.status.success(),
        "the bundled `{}` did not compile:\n{}",
        name,
        String::from_utf8_lossy(&rustc.stderr),
    );

    let mut child = Command::new(&exe)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().unwrap().write_all(stdin.as_bytes())?;
    let run = child.wait_with_output()?;
    anyhow::ensure!(
        run.status.success(),
        "the bundled `{}` exited with {}:\n{}",
        name,
        run.status,
        String::from_utf8_lossy(&run.stderr),
    );
    assert_eq!(
        expected_stdout,
        String::from_utf8_lossy(&run.stdout),
        "unexpected stdout from the bundled `{}`",
        name,
    );
    Ok(())
}

fn manifest_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).to_owned()
}
