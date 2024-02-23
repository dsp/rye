use std::borrow::Cow;
use std::env::consts::EXE_EXTENSION;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{self, AtomicBool};
use std::{env, fs};

use anyhow::{anyhow, bail, Context, Error};
use console::style;
use indicatif::{ProgressBar, ProgressStyle};
use once_cell::sync::Lazy;
use tempfile::NamedTempFile;

use crate::config::Config;
use crate::piptools::LATEST_PIP;
use crate::platform::{
    get_app_dir, get_canonical_py_path, get_toolchain_python_bin, list_known_toolchains,
};
use crate::pyproject::{latest_available_python_version, write_venv_marker};
use crate::sources::py::{get_download_url, PythonVersion, PythonVersionRequest};
use crate::sources::uv::{UvDownload, UvRequest};
use crate::utils::{
    check_checksum, set_proxy_variables, symlink_file, unpack_archive, CommandOutput, IoPathContext,
};

/// this is the target version that we want to fetch
pub const SELF_PYTHON_TARGET_VERSION: PythonVersionRequest = PythonVersionRequest {
    name: Some(Cow::Borrowed("cpython")),
    arch: None,
    os: None,
    major: 3,
    minor: Some(12),
    patch: None,
    suffix: None,
};

const SELF_VERSION: u64 = 14;

const SELF_REQUIREMENTS: &str = r#"
build==1.0.3
certifi==2023.11.17
charset-normalizer==3.3.2
click==8.1.7
distlib==0.3.8
filelock==3.12.2
idna==3.4
packaging==23.1
platformdirs==4.0.0
pyproject_hooks==1.0.0
requests==2.31.0
tomli==2.0.1
twine==4.0.2
unearth==0.14.0
urllib3==2.0.7
virtualenv==20.25.0
ruff==0.2.2
uv==0.1.9
"#;

static FORCED_TO_UPDATE: AtomicBool = AtomicBool::new(false);

fn is_up_to_date() -> bool {
    static UP_TO_UPDATE: Lazy<bool> = Lazy::new(|| {
        fs::read_to_string(get_app_dir().join("self").join("tool-version.txt"))
            .ok()
            .map_or(false, |x| x.parse() == Ok(SELF_VERSION))
    });
    *UP_TO_UPDATE || FORCED_TO_UPDATE.load(atomic::Ordering::Relaxed)
}

/// Bootstraps the venv for rye itself
pub fn ensure_self_venv(output: CommandOutput) -> Result<PathBuf, Error> {
    ensure_self_venv_with_toolchain(output, None)
}

/// Bootstraps the venv for rye itself
pub fn ensure_self_venv_with_toolchain(
    output: CommandOutput,
    toolchain_version_request: Option<PythonVersionRequest>,
) -> Result<PathBuf, Error> {
    let app_dir = get_app_dir();
    let venv_dir = app_dir.join("self");

    if venv_dir.is_dir() {
        if is_up_to_date() {
            return Ok(venv_dir);
        } else {
            if output != CommandOutput::Quiet {
                echo!("Detected outdated rye internals. Refreshing");
            }
            fs::remove_dir_all(&venv_dir)
                .path_context(&venv_dir, "could not remove self-venv for update")?;
        }
    }

    if output != CommandOutput::Quiet {
        echo!("Bootstrapping rye internals");
    }

    // Ensure we have uv
    let uv = Uv::ensure_exists(output)?;

    let version = match toolchain_version_request {
        Some(ref version_request) => ensure_specific_self_toolchain(output, version_request)
            .with_context(|| {
                format!(
                    "failed to provision internal cpython toolchain {}",
                    version_request
                )
            })?,
        None => ensure_latest_self_toolchain(output).with_context(|| {
            format!(
                "failed to fetch internal cpython toolchain {}",
                SELF_PYTHON_TARGET_VERSION
            )
        })?,
    };

    let py_bin = get_toolchain_python_bin(&version)?;

    // linux specific detection of shared libraries.
    #[cfg(target_os = "linux")]
    {
        validate_shared_libraries(&py_bin)?;
    }

    // initialize the virtualenv
    {
        let uv_venv = uv.venv(&venv_dir, &py_bin, &version)?;
        // write our marker
        uv_venv.write_marker()?;
        // update pip and our requirements
        uv_venv.update()?;

        // Update the shims
        let shims = app_dir.join("shims");
        if !shims.is_dir() {
            fs::create_dir_all(&shims).path_context(&shims, "tried to create shim folder")?;
        }

        // if rye is itself installed into the shims folder, we want to
        // use that.  Otherwise we fall back to the current executable
        let mut this = shims.join("rye").with_extension(EXE_EXTENSION);
        if !this.is_file() {
            this = env::current_exe()?;
        }

        update_core_shims(&shims, &this)?;

        uv_venv.write_tool_version(SELF_VERSION)?;
    }

    FORCED_TO_UPDATE.store(true, atomic::Ordering::Relaxed);

    Ok(venv_dir)
}

pub fn update_core_shims(shims: &Path, this: &Path) -> Result<(), Error> {
    #[cfg(unix)]
    {
        let py_shim = shims.join("python");
        let py3_shim = shims.join("python3");

        // on linux we cannot symlink at all, as this will misreport.  We will try to do
        // hardlinks and if that fails, we fall back to copying the entire file over.  This
        // for instance is needed when the rye executable is placed on a different volume
        // than ~/.rye/shims
        if cfg!(target_os = "linux") {
            fs::remove_file(&py_shim).ok();
            if fs::hard_link(this, &py_shim).is_err() {
                fs::copy(this, &py_shim).path_context(&py_shim, "tried to copy python shim")?;
            }
            fs::remove_file(&py3_shim).ok();
            if fs::hard_link(this, &py3_shim).is_err() {
                fs::copy(this, &py3_shim).path_context(&py_shim, "tried to copy python3 shim")?;
            }

        // on other unices we always use symlinks
        } else {
            fs::remove_file(&py_shim).ok();
            symlink_file(this, &py_shim).path_context(&py_shim, "tried to symlink python shim")?;
            fs::remove_file(&py3_shim).ok();
            symlink_file(this, &py3_shim)
                .path_context(&py3_shim, "tried to symlink python3 shim")?;
        }
    }

    #[cfg(windows)]
    {
        let py_shim = shims.join("python.exe");
        let pyw_shim = shims.join("pythonw.exe");
        let py3_shim = shims.join("python3.exe");

        // on windows we need privileges to symlink.  Not everyone might have that, so we
        // fall back to hardlinks.
        fs::remove_file(&py_shim).ok();
        if symlink_file(this, &py_shim).is_err() {
            fs::hard_link(this, &py_shim).path_context(&py_shim, "tried to symlink python shim")?;
        }
        fs::remove_file(&py3_shim).ok();
        if symlink_file(this, &py3_shim).is_err() {
            fs::hard_link(this, &py3_shim)
                .path_context(&py3_shim, "tried to symlink python3 shim")?;
        }
        fs::remove_file(&pyw_shim).ok();
        if symlink_file(this, &pyw_shim).is_err() {
            fs::hard_link(this, &pyw_shim)
                .path_context(&pyw_shim, "tried to symlink pythonw shim")?;
        }
    }

    Ok(())
}

/// Returns the pip runner for the self venv
pub fn get_pip_runner(venv: &Path) -> Result<PathBuf, Error> {
    Ok(get_pip_module(venv)?.join("__pip-runner__.py"))
}

/// Returns the pip module for the self venv
pub fn get_pip_module(venv: &Path) -> Result<PathBuf, Error> {
    let mut rv = venv.to_path_buf();
    rv.push("lib");
    #[cfg(windows)]
    {
        rv.push("site-packages");
    }
    #[cfg(unix)]
    {
        // This is not optimal.  We find the first thing that
        // looks like pythonX.X/site-packages and just use it.
        // It also means that this requires us to do some unnecessary
        // file system operations.  However given how hopefully
        // infrequent this function is called, we might be good.
        let dir = rv.read_dir()?;
        let mut found = false;
        for entry in dir.filter_map(|x| x.ok()) {
            let filename = entry.file_name();
            if let Some(filename) = filename.to_str() {
                if filename.starts_with("python") {
                    rv.push(filename);
                    rv.push("site-packages");
                    if rv.is_dir() {
                        found = true;
                        break;
                    } else {
                        rv.pop();
                        rv.pop();
                    }
                }
            }
        }
        if !found {
            bail!("no site-packages in venv");
        }
    }
    rv.push("pip");
    Ok(rv)
}

/// we only support cpython 3.9 to 3.12
pub fn is_self_compatible_toolchain(version: &PythonVersion) -> bool {
    version.name == "cpython" && version.major == 3 && version.minor >= 9 && version.minor <= 12
}

/// Ensure that the toolchain for the self environment is available.
fn ensure_latest_self_toolchain(output: CommandOutput) -> Result<PythonVersion, Error> {
    if let Some(version) = list_known_toolchains()?
        .into_iter()
        .map(|x| x.0)
        .filter(is_self_compatible_toolchain)
        .collect::<Vec<_>>()
        .into_iter()
        .max()
    {
        if output != CommandOutput::Quiet {
            echo!(
                "Found a compatible Python version: {}",
                style(&version).cyan()
            );
        }
        Ok(version)
    } else {
        fetch(&SELF_PYTHON_TARGET_VERSION, output)
    }
}

/// Ensure a specific toolchain is available.
fn ensure_specific_self_toolchain(
    output: CommandOutput,
    toolchain_version_request: &PythonVersionRequest,
) -> Result<PythonVersion, Error> {
    let toolchain_version = latest_available_python_version(toolchain_version_request)
        .ok_or_else(|| anyhow!("requested toolchain version is not available"))?;
    if !is_self_compatible_toolchain(&toolchain_version) {
        bail!(
            "the requested toolchain version ({}) is not supported for rye-internal usage",
            toolchain_version
        );
    }
    if !get_toolchain_python_bin(&toolchain_version)?.is_file() {
        if output != CommandOutput::Quiet {
            echo!(
                "Fetching requested internal toolchain '{}'",
                toolchain_version
            );
        }
        fetch(&toolchain_version.into(), output)
    } else {
        if output != CommandOutput::Quiet {
            echo!(
                "Found a compatible Python version: {}",
                style(&toolchain_version).cyan()
            );
        }
        Ok(toolchain_version)
    }
}

/// Fetches a version if missing.
pub fn fetch(
    version: &PythonVersionRequest,
    output: CommandOutput,
) -> Result<PythonVersion, Error> {
    if let Ok(version) = PythonVersion::try_from(version.clone()) {
        let py_bin = get_toolchain_python_bin(&version)?;
        if py_bin.is_file() {
            if output == CommandOutput::Verbose {
                echo!("Python version already downloaded. Skipping.");
            }
            return Ok(version);
        }
    }

    let (version, url, sha256) = match get_download_url(version) {
        Some(result) => result,
        None => bail!("unknown version {}", version),
    };

    let target_dir = get_canonical_py_path(&version)?;
    let target_py_bin = get_toolchain_python_bin(&version)?;
    if output == CommandOutput::Verbose {
        echo!("target dir: {}", target_dir.display());
    }
    if target_dir.is_dir() && target_py_bin.is_file() {
        if output == CommandOutput::Verbose {
            echo!("Python version already downloaded. Skipping.");
        }
        return Ok(version);
    }

    fs::create_dir_all(&target_dir).path_context(&target_dir, "failed to create target folder")?;

    if output == CommandOutput::Verbose {
        echo!("download url: {}", url);
    }
    if output != CommandOutput::Quiet {
        echo!("{} {}", style("Downloading").cyan(), version);
    }
    let archive_buffer = download_url(url, output)?;

    if let Some(sha256) = sha256 {
        if output != CommandOutput::Quiet {
            echo!("{} {}", style("Checking").cyan(), "checksum");
        }
        check_checksum(&archive_buffer, sha256)
            .with_context(|| format!("Checksum check of {} failed", &url))?;
    } else if output != CommandOutput::Quiet {
        echo!("Checksum check skipped (no hash available)");
    }

    if output != CommandOutput::Quiet {
        echo!("{}", style("Unpacking").cyan());
    }
    unpack_archive(&archive_buffer, &target_dir, 1).with_context(|| {
        format!(
            "unpacking of downloaded tarball {} to '{}' failed",
            &url,
            target_dir.display()
        )
    })?;

    if output != CommandOutput::Quiet {
        echo!("{} {}", style("Downloaded").green(), version);
    }

    Ok(version)
}

pub fn download_url(url: &str, output: CommandOutput) -> Result<Vec<u8>, Error> {
    match download_url_ignore_404(url, output)? {
        Some(result) => Ok(result),
        None => bail!("Failed to download: 404 not found"),
    }
}

pub fn download_url_ignore_404(url: &str, output: CommandOutput) -> Result<Option<Vec<u8>>, Error> {
    // for now we only allow HTTPS downloads.
    if !url.starts_with("https://") {
        bail!("Refusing insecure download");
    }

    let config = Config::current();
    let mut archive_buffer = Vec::new();
    let mut handle = curl::easy::Easy::new();
    handle.url(url)?;
    handle.progress(true)?;
    handle.follow_location(true)?;

    // we only do https requests here, so we always set an https proxy
    if let Some(proxy) = config.https_proxy_url() {
        handle.proxy(&proxy)?;
    }

    // on windows we want to disable revocation checks.  The reason is that MITM proxies
    // will otherwise not work.  This is a schannel specific behavior anyways.
    // for more information see https://github.com/curl/curl/issues/264
    #[cfg(windows)]
    {
        handle.ssl_options(curl::easy::SslOpt::new().no_revoke(true))?;
    }

    let write_archive = &mut archive_buffer;
    {
        let mut transfer = handle.transfer();
        let mut pb = None;
        transfer.progress_function(move |a, b, _, _| {
            if output == CommandOutput::Quiet {
                return true;
            }

            let (down_len, down_pos) = (a as u64, b as u64);
            if down_len > 0 {
                if down_pos < down_len {
                    if pb.is_none() {
                        let pb_config = ProgressBar::new(down_len);
                        pb_config.set_style(
                            ProgressStyle::with_template("{wide_bar} {bytes:>7}/{total_bytes:7}")
                                .unwrap(),
                        );
                        pb = Some(pb_config);
                    }
                    pb.as_ref().unwrap().set_position(down_pos);
                } else if pb.is_some() {
                    pb.take().unwrap().finish_and_clear();
                }
            }
            true
        })?;
        transfer.write_function(move |data| {
            write_archive.write_all(data).unwrap();
            Ok(data.len())
        })?;
        transfer
            .perform()
            .with_context(|| format!("download of {} failed", &url))?;
    }
    let code = handle.response_code()?;
    if code == 404 {
        Ok(None)
    } else if !(200..300).contains(&code) {
        bail!("Failed to download: {}", code)
    } else {
        Ok(Some(archive_buffer))
    }
}

// Represents a uv binary and associated functions
// to bootstrap rye using uv.
#[derive(Clone)]
struct Uv {
    output: CommandOutput,
    uv_bin: PathBuf,
}

impl Uv {
    // Ensure we have a uv binary for bootstrapping
    fn ensure_exists(output: CommandOutput) -> Result<Self, Error> {
        // Request a download for the default uv binary for this platform.
        // For instance on aarch64 macos this will request a compatible uv version.
        let download = UvDownload::try_from(UvRequest::default())?;
        let uv_dir = get_app_dir().join("uv").join(download.version());
        let uv_bin = uv_dir.join("uv");

        if uv_dir.exists() && uv_bin.is_file() {
            return Ok(Self { uv_bin, output });
        }

        Self::download(&download, &uv_dir, output)?;
        if uv_dir.exists() && uv_bin.is_file() {
            return Ok(Self { uv_bin, output });
        }

        Err(anyhow!("Failed to ensure uv binary is available"))
    }

    fn download(download: &UvDownload, uv_dir: &Path, output: CommandOutput) -> Result<(), Error> {
        // Download the version
        let archive_buffer = download_url(&download.url, output)?;

        // All uv downloads must have a sha256 checksum
        check_checksum(&archive_buffer, &download.sha256)
            .with_context(|| format!("Checksum check of {} failed", download.url))?;

        // Unpack the archive once we ensured that the checksum is correct
        unpack_archive(&archive_buffer, uv_dir, 1).with_context(|| {
            format!(
                "unpacking of downloaded tarball {} to '{}' failed",
                download.url,
                uv_dir.display(),
            )
        })?;

        Ok(())
    }

    fn cmd(&self) -> Command {
        let mut cmd = Command::new(&self.uv_bin);

        match self.output {
            CommandOutput::Verbose => {
                cmd.arg("--verbose");
            }
            CommandOutput::Quiet => {
                cmd.arg("--quiet");
                cmd.env("PYTHONWARNINGS", "ignore");
            }
            CommandOutput::Normal => {}
        }

        set_proxy_variables(&mut cmd);
        cmd
    }

    // Generate a venv using the uv binary
    fn venv(
        &self,
        venv_dir: &Path,
        py_bin: &Path,
        version: &PythonVersion,
    ) -> Result<UvWithVenv, Error> {
        self.cmd()
            .arg("venv")
            .arg("--python")
            .arg(py_bin)
            .arg(venv_dir)
            .status()
            .with_context(|| {
                format!(
                    "unable to create self venv using {}. It might be that \
                      the used Python build is incompatible with this machine. \
                      For more information see https://rye-up.com/guide/installation/",
                    py_bin.display()
                )
            })?;

        Ok(UvWithVenv::new(self.clone(), venv_dir, version))
    }
}

// Represents a venv generated and managed by uv
struct UvWithVenv {
    uv: Uv,
    venv_path: PathBuf,
    py_version: PythonVersion,
}

impl UvWithVenv {
    fn new(uv: Uv, venv_dir: &Path, version: &PythonVersion) -> Self {
        UvWithVenv {
            uv,
            py_version: version.clone(),
            venv_path: venv_dir.to_path_buf(),
        }
    }

    fn venv_cmd(&self) -> Command {
        let mut cmd = self.uv.cmd();
        cmd.env("VIRTUAL_ENV", &self.venv_path);
        cmd
    }

    fn write_marker(&self) -> Result<(), Error> {
        write_venv_marker(&self.venv_path, &self.py_version)
    }

    fn update(&self) -> Result<(), Error> {
        self.update_pip(LATEST_PIP)?;
        self.update_requirements(SELF_REQUIREMENTS)?;
        Ok(())
    }

    fn update_pip(&self, pip_version: &str) -> Result<(), Error> {
        self.venv_cmd()
            .arg("pip")
            .arg("install")
            .arg("--upgrade")
            .arg(pip_version)
            .status()
            .with_context(|| {
                format!(
                    "unable to update pip in venv at {}",
                    self.venv_path.display()
                )
            })?;

        Ok(())
    }

    fn update_requirements(&self, requirements: &str) -> Result<(), Error> {
        let mut req_file = NamedTempFile::new()?;
        writeln!(req_file, "{}", requirements)?;

        self.venv_cmd()
            .arg("pip")
            .arg("install")
            .arg("--upgrade")
            .arg("-r")
            .arg(req_file.path())
            .status()
            .with_context(|| {
                format!(
                    "unable to update requirements in venv at {}",
                    self.venv_path.display()
                )
            })?;

        Ok(())
    }

    fn write_tool_version(&self, version: u64) -> Result<(), Error> {
        let tool_version_path = self.venv_path.join("tool-version.txt");
        fs::write(&tool_version_path, version.to_string())
            .path_context(&tool_version_path, "could not write tool version")?;
        Ok(())
    }
}

#[cfg(target_os = "linux")]
fn validate_shared_libraries(py: &Path) -> Result<(), Error> {
    use std::process::Command;
    let out = Command::new("ldd")
        .arg(py)
        .output()
        .context("unable to invoke ldd on downloaded python binary")?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    let mut missing = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if let Some((before, after)) = line.split_once(" => ") {
            if after == "not found" && !missing.contains(&before) {
                missing.push(before);
            }
        }
    }

    if missing.is_empty() {
        return Ok(());
    }

    missing.sort();
    echo!(
        "{}: detected missing shared librar{} required by Python:",
        style("error").red(),
        if missing.len() == 1 { "y" } else { "ies" }
    );
    for lib in missing {
        echo!("  - {}", style(lib).yellow());
    }
    bail!(
        "Python installation is unable to run on this machine due to missing libraries.\n\
        Visit https://rye-up.com/guide/faq/#missing-shared-libraries-on-linux for next steps."
    );
}
