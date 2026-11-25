// Runner for the private agents/ test suite.
//
// `agents/` lives in the private studio repo and is gitignored, so it is
// absent on fresh contributor clones. `npm run verify` is the
// contributor-safe subset: this runner skips cleanly (exit 0) when
// agents/tests is missing and runs `node --test` on the suite when the
// studio checkout is present. The full superset is `npm run verify:full`.
import { existsSync, readdirSync } from "node:fs";
import { spawnSync } from "node:child_process";

const agentsTestsDir = new URL("../agents/tests", import.meta.url);

if (!existsSync(agentsTestsDir)) {
  console.log(
    "test:agents: skip — agents/ is not checked out (private studio repo absent); run verify:full from the studio checkout"
  );
  process.exit(0);
}

const files = readdirSync(agentsTestsDir).filter((f) => f.endsWith(".test.mjs"));
if (files.length === 0) {
  console.log("test:agents: skip — agents/tests has no *.test.mjs files");
  process.exit(0);
}

const result = spawnSync(process.execPath, ["--test", ...files], {
  cwd: agentsTestsDir,
  stdio: "inherit",
});

process.exit(result.status ?? 1);
