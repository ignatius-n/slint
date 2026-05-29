// Copyright © SixtyFPS GmbH <info@slint.dev>
// SPDX-License-Identifier: MIT
//
// Generate a Starlight "Third-Party Licenses" Markdown page from the Markdown
// emitted by `cargo xtask license`. Shared by the per-language docs sites
// (docs/cpp, docs/nodejs, docs/python), which differ only in the crate
// directory and the output path.
import { spawnSync } from "node:child_process";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { dirname } from "node:path";

const FRONT_MATTER = [
    "---",
    "title: Third-Party Licenses",
    "slug: thirdparty",
    // Each license is an `h2`; limit the on-page navigation to those so it
    // lists the licenses without the per-license "Used by"/"License Text"
    // subheadings.
    "tableOfContents:",
    "    maxHeadingLevel: 2",
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

    // Let the generator write the Markdown body directly to the output file:
    // the listing easily exceeds spawnSync's default stdout buffer, and
    // inheriting stdio keeps the build log and any error visible.
    const result = spawnSync("cargo", ["xtask", "license", "-o", outFile], {
        cwd: crateDir,
        stdio: "inherit",
    });
    if (result.status !== 0) {
        process.exit(result.status ?? 1);
    }

    // Prepend the Starlight front matter the docs site needs (the body itself
    // is shared verbatim with the binary packages, which must not carry it).
    const body = readFileSync(outFile, "utf8");
    writeFileSync(outFile, FRONT_MATTER + body, "utf8");
}
