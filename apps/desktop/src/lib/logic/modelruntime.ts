import type { ModelRowDto } from "../types/dto";

/**
 * Model/runtime rendering consumes the backend-joined row directly. A webview
 * must not rediscover which role wants a model, match role-level slots, or
 * infer provider truth from hardware capability.
 */
export function modelRuntimeStatus(model: ModelRowDto): string {
  const consumers = model.consumers.filter(
    (consumer) => consumer.desired || consumer.active,
  );
  return consumers
    .map((consumer) => {
      const state = consumer.state === "ready" ? "running" : consumer.state;
      return `${consumer.role} ${state}`;
    })
    .join(", ");
}

export function modelRuntimeError(model: ModelRowDto): string {
  return [
    ...new Set(
      model.consumers
        .map((consumer) => consumer.error)
        .filter((error): error is string => error !== null && error !== ""),
    ),
  ].join("; ");
}

export function modelExecutionStatus(model: ModelRowDto): string {
  const details = model.consumers
    .filter((consumer) => consumer.active)
    .map((consumer) => {
      const actual =
        consumer.actualProvider === null
          ? "actual provider unavailable"
          : `actual provider ${consumer.actualProvider}`;
      const requested =
        consumer.requestedProvider === null
          ? ""
          : ` (requested ${consumer.requestedProvider})`;
      const fallback =
        consumer.fallbackReason === null ? "" : `; ${consumer.fallbackReason}`;
      return `${consumer.role}: ${actual}${requested}${fallback}`;
    });
  return details.join("; ");
}
