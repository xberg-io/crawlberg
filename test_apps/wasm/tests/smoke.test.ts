import { describe, expect, it } from "vitest";
import * as pkg from "@xberg-io/crawlberg-wasm";

describe("smoke", () => {
    it("imports the published wasm package", () => {
        expect(pkg).toBeDefined();
        expect(typeof pkg).toBe("object");
    });
});
