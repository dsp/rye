use insta::assert_snapshot;

use crate::common::{rye_cmd_snapshot, Space};

mod common;

#[test]
#[cfg(all(target_os = "linux", target_env = "musl"))]
fn test_bootstrap_linux_musl() {
    let space = Space::new();
    space.init("my-project");
    rye_cmd_snapshot!(space.rye_cmd().arg("config").arg("--set").arg("default.toolchain=cpython-x86_64-linux-musl@3.12.1"), @r###"
    success: true
    exit_code: 0
    ----- stdout -----

    ----- stderr -----
    "###);

    rye_cmd_snapshot!(space.rye_cmd().arg("sync"), @r###"
    success: true
    exit_code: 0
    ----- stdout -----
    Initializing new virtualenv in [TEMP_PATH]/project/.venv
    Python version: cpython@3.12.1
    Generating production lockfile: [TEMP_PATH]/project/requirements.lock
    Generating dev lockfile: [TEMP_PATH]/project/requirements-dev.lock
    Installing dependencies
    Done!

    ----- stderr -----
    warning: Requirements file [TEMP_FILE] does not contain any dependencies
    Built 1 editable in [EXECUTION_TIME]
    Resolved 1 package in [EXECUTION_TIME]
    warning: Requirements file [TEMP_FILE] does not contain any dependencies
    Built 1 editable in [EXECUTION_TIME]
    Resolved 1 package in [EXECUTION_TIME]
    Built 1 editable in [EXECUTION_TIME]
    Installed 1 package in [EXECUTION_TIME]
     + my-project==0.1.0 (from file:[TEMP_PATH]/project)
    "###);

    rye_cmd_snapshot!(space.rye_cmd().arg("run"), @r###"
    success: true
    exit_code: 0
    ----- stdout -----

    ----- stderr -----
    hello
    python
    python3
    python3.12

    "###);

    // NOTE: Due to #726 hello will currently exit with 1 and print to stderr.
    rye_cmd_snapshot!(space.rye_cmd().arg("run").arg("hello"), @r###"
    success: false
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    Hello from my-project!
    "###);

    space.write(
        "src/my_project/__init__.py", r#"
import sysconfig
def hello():
    cc = sysconfig.get_config_var('CC')
    linkcc = sysconfig.get_config_var('LINKCC')
    if 'musl' in cc and 'musl' in linkcc:
        return 0
    else:
        return 1
"#);

    rye_cmd_snapshot!(space.rye_cmd().arg("run").arg("hello"), @r###"
    success: true
    exit_code: 0
    ----- stdout -----

    ----- stderr -----
    "###);
}

#[test]
#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn test_bootstrap_linux_gnu() {
    let space = Space::new();
    space.init("my-project");
    rye_cmd_snapshot!(space.rye_cmd().arg("config").arg("--set").arg("default.toolchain=cpython-x86_64-linux-gnu@3.12.1"), @r###"
    success: true
    exit_code: 0
    ----- stdout -----

    ----- stderr -----
    "###);

    rye_cmd_snapshot!(space.rye_cmd().arg("sync"), @r###"
    success: true
    exit_code: 0
    ----- stdout -----
    Initializing new virtualenv in [TEMP_PATH]/project/.venv
    Python version: cpython@3.12.1
    Generating production lockfile: [TEMP_PATH]/project/requirements.lock
    Generating dev lockfile: [TEMP_PATH]/project/requirements-dev.lock
    Installing dependencies
    Done!

    ----- stderr -----
    warning: Requirements file [TEMP_FILE] does not contain any dependencies
    Built 1 editable in [EXECUTION_TIME]
    Resolved 1 package in [EXECUTION_TIME]
    warning: Requirements file [TEMP_FILE] does not contain any dependencies
    Built 1 editable in [EXECUTION_TIME]
    Resolved 1 package in [EXECUTION_TIME]
    Built 1 editable in [EXECUTION_TIME]
    Installed 1 package in [EXECUTION_TIME]
     + my-project==0.1.0 (from file:[TEMP_PATH]/project)
    "###);

    rye_cmd_snapshot!(space.rye_cmd().arg("run"), @r###"
    success: true
    exit_code: 0
    ----- stdout -----

    ----- stderr -----
    hello
    python
    python3
    python3.12

    "###);

    // NOTE: Due to #726 hello will currently exit with 1 and print to stderr.
    rye_cmd_snapshot!(space.rye_cmd().arg("run").arg("hello"), @r###"
    success: false
    exit_code: 1
    ----- stdout -----

    ----- stderr -----
    Hello from my-project!
    "###);
}
