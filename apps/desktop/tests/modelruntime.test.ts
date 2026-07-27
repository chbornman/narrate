import { describe, expect, it } from "vitest";
import {
  modelExecutionStatus,
  modelRuntimeError,
  modelRuntimeStatus,
} from "../src/lib/logic/modelruntime";
import type { ModelRowDto } from "../src/lib/types/dto";

function model(over: Partial<ModelRowDto> = {}): ModelRowDto {
  return {
    id: "future-text-variant",
    role: "text-embedder",
    defaultOffer: true,
    advancedAvailable: false,
    compatible: true,
    compatibilityReason: "CPU compatible",
    compatibleProviders: ["CPU", "CUDA"],
    consumers: [
      {
        role: "text-embedder",
        desired: true,
        active: true,
        state: "ready",
        retryable: false,
        error: null,
        requestedProvider: "cuda",
        actualProvider: "cpu",
        fallbackReason: "graph fell back to CPU",
      },
    ],
    state: "installed",
    totalBytes: 10,
    downloadedBytes: 10,
    licenseName: "fixture",
    licenseUrl: "https://example.test",
    acceptanceRequired: false,
    accepted: true,
    error: null,
    retryHint: null,
    operation: null,
    operationEvent: null,
    registryError: null,
    ...over,
  };
}

describe("backend-joined model runtime truth", () => {
  it("renders arbitrary model roles without a frontend model-id allowlist", () => {
    expect(modelRuntimeStatus(model())).toBe("text-embedder running");
    expect(
      modelRuntimeStatus(
        model({
          consumers: [
            {
              role: "future-role",
              desired: true,
              active: false,
              state: "queued",
              retryable: false,
              error: null,
              requestedProvider: null,
              actualProvider: null,
              fallbackReason: null,
            },
          ],
        }),
      ),
    ).toBe("future-role queued");
  });

  it("surfaces backend failure and actual execution provider verbatim", () => {
    expect(modelRuntimeError(model())).toBe("");
    expect(modelExecutionStatus(model())).toBe(
      "text-embedder: actual provider cpu (requested cuda); graph fell back to CPU",
    );
    const failed = model({
      consumers: [
        {
          role: "clip",
          desired: true,
          active: true,
          state: "failed",
          retryable: true,
          error: "CUDA provider initialization failed",
          requestedProvider: "cuda",
          actualProvider: null,
          fallbackReason: null,
        },
      ],
    });
    expect(modelRuntimeError(failed)).toBe("CUDA provider initialization failed");
  });
});
