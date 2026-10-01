// Fixture (P5, subprocess): JavaScript starting repository scripts.
import { spawn, execSync } from "child_process";

export function build() {
  spawn("python", ["scripts/build.py"]); // repository file -> possible
  execSync("sh helper.sh"); // `helper.sh` exists in a/ and b/: ambiguous path -> no bridge
  spawn("ls", ["-la"]); // negative control: no repository script
}
