// Builds the CLI and puts it where tauri's `externalBin` expects it. Run by
// `beforeBuildCommand`, which sets TAURI_ENV_TARGET_TRIPLE for `tauri build --target`.
import { execFileSync } from "node:child_process";
import { copyFileSync, mkdirSync } from "node:fs";
import path from "node:path";
import process from "node:process";

const desktop = path.resolve(import.meta.dirname, "..");
const workspace = path.resolve(desktop, "../..");

function hostTriple() {
  const line = execFileSync("rustc", ["-vV"], { encoding: "utf8" })
    .split("\n")
    .find((text) => text.startsWith("host: "));
  if (!line) throw new Error("rustc -vV printed no host triple");
  return line.slice("host: ".length).trim();
}

const requested = process.env.TAURI_ENV_TARGET_TRIPLE;
const triple = requested || hostTriple();
const extension = triple.includes("windows") ? ".exe" : "";

const target = requested ? ["--target", requested] : [];
execFileSync("cargo", ["build", "--release", "-p", "jobpilot-cli", ...target], {
  cwd: workspace,
  stdio: "inherit",
});

const built = requested
  ? path.join(workspace, "target", requested, "release", `jobpilot${extension}`)
  : path.join(workspace, "target", "release", `jobpilot${extension}`);
const binaries = path.join(desktop, "src-tauri", "binaries");
const destination = path.join(binaries, `jobpilot-${triple}${extension}`);
mkdirSync(binaries, { recursive: true });
copyFileSync(built, destination);
console.log(`sidecar: ${destination}`);
