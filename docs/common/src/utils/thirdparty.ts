// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: MIT
//
// Generate a Starlight "Third-Party Licenses" Markdown page from the Markdown
// emitted by `cargo xtask license`. Shared by the per-language docs sites
// (docs/cpp, docs/nodejs, docs/python), which differ only in the crate
// directory and the output path.
import { spawnSync } from "node:child_process";
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";

const FRONT_MATTER = [
    "---",
    "title: Third-Party Licenses",
    "slug: thirdparty",
    "tableOfContents: false",
    "---",
    "",
    "",
].join("\n");

/**
 * Run `cargo xtask license` in `crateDir` and write the rendered Third-Party
 * Licenses Markdown page to `outFile`. Exits the process if the generator
 * fails.
 */
export function generateThirdPartyMarkdown(options: {
    crateDir: string;
    outFile: string;
}): void {
    const { crateDir, outFile } = options;
    mkdirSync(dirname(outFile), { recursive: true });

    const result = spawnSync("cargo", ["xtask", "license"], {
        cwd: crateDir,
        encoding: "utf8",
    });
    if (result.status !== 0) {
        process.stderr.write(result.stderr ?? "");
        process.exit(result.status ?? 1);
    }

    writeFileSync(outFile, FRONT_MATTER + result.stdout, "utf8");
}
