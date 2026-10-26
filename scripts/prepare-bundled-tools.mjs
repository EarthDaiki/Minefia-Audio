import { createHash } from "node:crypto";
import { spawnSync } from "node:child_process";
import {
  cpSync,
  existsSync,
  mkdirSync,
  mkdtempSync,
  readFileSync,
  readdirSync,
  rmSync,
  writeFileSync,
} from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";

const projectRoot = path.resolve(path.dirname(fileURLToPath(import.meta.url)), "..");
const toolsRoot = path.join(projectRoot, "src-tauri", "bundled-tools");
const target = process.env.TAURI_ENV_TARGET_TRIPLE ?? process.env.RUST_TARGET ?? "";
const platform = target.includes("apple-darwin")
  ? "darwin"
  : target.includes("windows")
    ? "win32"
    : process.platform;
const architecture = target.startsWith("aarch64-") || target.startsWith("arm64-")
  ? "arm64"
  : target.startsWith("x86_64-")
    ? "x64"
    : process.arch === "arm64"
      ? "arm64"
      : "x64";
const ytdlpVersion = "2026.10.21";
const windowsFfmpegRelease = "autobuild-2026-10-22-13-06";
const toolSetVersion = platform === "darwin"
  ? `ffmpeg-macos-9.0.1-1-ytdlp-${ytdlpVersion}`
  : `ffmpeg-windows-${windowsFfmpegRelease}-ytdlp-${ytdlpVersion}`;
const ytdlpRelease = `https://github.com/yt-dlp/yt-dlp/releases/download/${ytdlpVersion}`;
const ytdlpAsset = platform === "darwin" ? "yt-dlp_macos" : architecture === "arm64" ? "yt-dlp_arm64.exe" : "yt-dlp.exe";
const executableSuffix = platform === "win32" ? ".exe" : "";

function log(message) {
  process.stdout.write(`[bundled-tools] ${message}\n`);
}

async function download(url) {
  const response = await fetch(url, { headers: { "User-Agent": "Minefia-Audio-build" } });
  if (!response.ok) throw new Error(`Download failed (${response.status}): ${url}`);
  return Buffer.from(await response.arrayBuffer());
}

function prepareDirectory() {
  mkdirSync(toolsRoot, { recursive: true });
  const targetMarker = path.join(toolsRoot, ".target-version");
  const expectedMarker = `${platform}/${architecture}/${toolSetVersion}`;
  const ytDlpPath = path.join(toolsRoot, "yt-dlp", `yt-dlp${executableSuffix}`);
  const ffmpegPath = path.join(toolsRoot, "ffmpeg", "bin", `ffmpeg${executableSuffix}`);
  const ffprobePath = path.join(toolsRoot, "ffmpeg", "bin", `ffprobe${executableSuffix}`);
  if (process.env.MINEFIA_FORCE_TOOL_REFRESH !== "1"
      && existsSync(ytDlpPath) && existsSync(ffmpegPath) && existsSync(ffprobePath)
      && existsSync(targetMarker) && readFileSync(targetMarker, "utf8") === expectedMarker) {
    log(`Reusing staged tools for ${platform}/${architecture}. Set MINEFIA_FORCE_TOOL_REFRESH=1 to redownload.`);
    return true;
  }
  for (const entry of readdirSync(toolsRoot)) {
    if (entry !== ".gitkeep") rmSync(path.join(toolsRoot, entry), { recursive: true, force: true });
  }
  mkdirSync(path.join(toolsRoot, "ffmpeg", "bin"), { recursive: true });
  mkdirSync(path.join(toolsRoot, "yt-dlp"), { recursive: true });
  return false;
}

function ensureMacFfmpeg() {
  const packageName = `@dfloresc/ffmpeg-macos-lgpl-${architecture}`;
  const packageDir = path.join(projectRoot, "node_modules", "@dfloresc", `ffmpeg-macos-lgpl-${architecture}`);
  if (!existsSync(path.join(packageDir, "bin", "ffmpeg"))) {
    throw new Error(`Missing ${packageName}. Run npm install before building.`);
  }
  cpSync(path.join(packageDir, "bin"), path.join(toolsRoot, "ffmpeg", "bin"), { recursive: true });
  cpSync(path.join(packageDir, "lib"), path.join(toolsRoot, "ffmpeg", "lib"), { recursive: true });
  cpSync(path.join(packageDir, "LICENSES"), path.join(toolsRoot, "ffmpeg", "LICENSES"), { recursive: true });
  for (const file of ["BUILDINFO.txt", "README.md"]) {
    cpSync(path.join(packageDir, file), path.join(toolsRoot, "ffmpeg", file));
  }
  for (const name of ["ffmpeg", "ffprobe"]) {
    const chmod = spawnSync("chmod", ["755", path.join(toolsRoot, "ffmpeg", "bin", name)]);
    if (chmod.status !== 0) throw new Error(`Could not mark bundled ${name} executable.`);
  }
  log(`Prepared LGPL FFmpeg ${readFileSync(path.join(packageDir, "BUILDINFO.txt"), "utf8").match(/FFmpeg version: (.+)/)?.[1] ?? ""} (${architecture}).`);
}

async function ensureWindowsFfmpeg() {
  const releaseTag = windowsFfmpegRelease;
  const asset = architecture === "arm64"
    ? "ffmpeg-n9.0-latest-winarm64-lgpl-shared-9.0.zip"
    : "ffmpeg-n9.0-latest-win64-lgpl-shared-9.0.zip";
  const releaseUrl = `https://github.com/BtbN/FFmpeg-Builds/releases/download/${releaseTag}/${asset}`;
  const apiUrl = `https://api.github.com/repos/BtbN/FFmpeg-Builds/releases/tags/${releaseTag}`;
  const releaseResponse = await fetch(apiUrl, { headers: { "User-Agent": "Minefia-Audio-build", Accept: "application/vnd.github+json" } });
  if (!releaseResponse.ok) throw new Error(`Could not verify the pinned FFmpeg release (${releaseResponse.status}).`);
  const release = await releaseResponse.json();
  const metadata = release.assets?.find((item) => item.name === asset);
  if (!metadata?.digest?.startsWith("sha256:")) {
    throw new Error(`The pinned FFmpeg release does not publish a SHA-256 digest for ${asset}.`);
  }
  const archive = await download(releaseUrl);
  const actualDigest = createHash("sha256").update(archive).digest("hex");
  if (actualDigest !== metadata.digest.slice("sha256:".length)) {
    throw new Error("The downloaded FFmpeg archive failed its SHA-256 check.");
  }

  const tempDir = mkdtempSync(path.join(os.tmpdir(), "minefia-ffmpeg-"));
  try {
    const archivePath = path.join(tempDir, asset);
    writeFileSync(archivePath, archive);
    const extracted = spawnSync("tar", ["-xf", archivePath, "-C", tempDir], { stdio: "inherit" });
    if (extracted.status !== 0) throw new Error("Could not unpack the LGPL FFmpeg archive (tar failed).");
    const binDir = findDirectoryWith(tempDir, "ffmpeg.exe");
    if (!binDir || !existsSync(path.join(binDir, "ffprobe.exe"))) {
      throw new Error("The downloaded archive did not contain ffmpeg.exe and ffprobe.exe.");
    }
    cpSync(binDir, path.join(toolsRoot, "ffmpeg", "bin"), { recursive: true });
    copyLicenseFiles(path.dirname(binDir), path.join(toolsRoot, "ffmpeg", "LICENSES", "upstream"));
    const licenseDir = path.join(toolsRoot, "ffmpeg", "LICENSES", "ffmpeg");
    mkdirSync(licenseDir, { recursive: true });
    writeFileSync(
      path.join(licenseDir, "COPYING.LGPLv2.1"),
      await download("https://raw.githubusercontent.com/FFmpeg/FFmpeg/n9.0/COPYING.LGPLv2.1"),
    );
    writeFileSync(
      path.join(path.dirname(licenseDir), "NOTICE.txt"),
      `FFmpeg LGPL shared build from BtbN/FFmpeg-Builds (${releaseTag}).\nSee BUILDINFO.txt for the exact release asset and source references.\n`,
    );
    writeFileSync(
      path.join(toolsRoot, "ffmpeg", "BUILDINFO.txt"),
      `FFmpeg LGPL shared build\nRelease: ${releaseTag}\nAsset: ${asset}\nSHA-256: ${actualDigest}\nSource/build project: https://github.com/BtbN/FFmpeg-Builds/tree/${releaseTag}\nFFmpeg source: https://github.com/FFmpeg/FFmpeg/tree/n9.0\n\nThis is an LGPL shared build. The archive's adjacent DLLs are required and are bundled with ffmpeg.exe and ffprobe.exe.\n`,
    );
    log(`Prepared checksum-verified LGPL FFmpeg (${architecture}).`);
  } finally {
    rmSync(tempDir, { recursive: true, force: true });
  }
}

function findDirectoryWith(root, filename) {
  for (const entry of readdirSync(root, { withFileTypes: true })) {
    const fullPath = path.join(root, entry.name);
    if (entry.isFile() && entry.name === filename) return root;
    if (entry.isDirectory()) {
      const found = findDirectoryWith(fullPath, filename);
      if (found) return found;
    }
  }
  return null;
}

function copyLicenseFiles(source, destination) {
  for (const entry of readdirSync(source, { withFileTypes: true })) {
    const fullPath = path.join(source, entry.name);
    if (entry.isDirectory()) {
      copyLicenseFiles(fullPath, path.join(destination, entry.name));
    } else if (entry.isFile() && /^(LICENSE|COPYING|PATENTS)(\.|$)/i.test(entry.name)) {
      mkdirSync(destination, { recursive: true });
      cpSync(fullPath, path.join(destination, entry.name));
    }
  }
}

async function prepareYtDlp() {
  const sumsResponse = await fetch(`${ytdlpRelease}/SHA2-256SUMS`, { headers: { "User-Agent": "Minefia-Audio-build" } });
  if (!sumsResponse.ok) throw new Error(`Could not retrieve the pinned yt-dlp checksum manifest (${sumsResponse.status}).`);
  const sums = await sumsResponse.text();
  const expected = sums.match(new RegExp(`^([a-f0-9]{64})\\s+\\*?${ytdlpAsset.replaceAll(".", "\\.")}\\s*$`, "m"))?.[1];
  if (!expected) throw new Error(`No SHA-256 entry for ${ytdlpAsset} in the pinned yt-dlp release.`);
  const binary = await download(`${ytdlpRelease}/${ytdlpAsset}`);
  const actual = createHash("sha256").update(binary).digest("hex");
  if (actual !== expected) throw new Error("The downloaded yt-dlp binary failed its SHA-256 check.");

  const executablePath = path.join(toolsRoot, "yt-dlp", `yt-dlp${executableSuffix}`);
  writeFileSync(executablePath, binary, { mode: 0o755 });
  const [thirdParty, license] = await Promise.all([
    download(`https://raw.githubusercontent.com/yt-dlp/yt-dlp/${ytdlpVersion}/THIRD_PARTY_LICENSES.txt`),
    download(`https://raw.githubusercontent.com/yt-dlp/yt-dlp/${ytdlpVersion}/LICENSE`),
  ]);
  writeFileSync(path.join(toolsRoot, "yt-dlp", "THIRD_PARTY_LICENSES.txt"), thirdParty);
  writeFileSync(path.join(toolsRoot, "yt-dlp", "LICENSE"), license);
  writeFileSync(path.join(toolsRoot, "yt-dlp", "BUILDINFO.txt"), `yt-dlp ${ytdlpVersion}\nAsset: ${ytdlpAsset}\nSHA-256: ${actual}\nLicense: GPL-3.0-or-later (standalone PyInstaller build)\nSource: https://github.com/yt-dlp/yt-dlp/tree/${ytdlpVersion}\n`);
  log(`Prepared checksum-verified yt-dlp ${ytdlpVersion} (${architecture}).`);
}

async function main() {
  if (platform !== "darwin" && platform !== "win32") {
    throw new Error(`Bundled tools are prepared for macOS and Windows, not ${platform}.`);
  }
  if (platform === "win32" && architecture !== "x64" && architecture !== "arm64") {
    throw new Error(`Unsupported Windows architecture: ${architecture}.`);
  }
  if (platform === "darwin" && architecture !== "x64" && architecture !== "arm64") {
    throw new Error(`Unsupported macOS architecture: ${architecture}.`);
  }

  if (prepareDirectory()) return;
  if (platform === "darwin") ensureMacFfmpeg();
  else await ensureWindowsFfmpeg();
  await prepareYtDlp();
  writeFileSync(path.join(toolsRoot, ".target-version"), `${platform}/${architecture}/${toolSetVersion}`);
  log(`Tools staged under ${path.relative(projectRoot, toolsRoot)} for ${platform}/${architecture}.`);
}

main().catch((error) => {
  process.stderr.write(`[bundled-tools] ERROR: ${error.message}\n`);
  process.exitCode = 1;
});
