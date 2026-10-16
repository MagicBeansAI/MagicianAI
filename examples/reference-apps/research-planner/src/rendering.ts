import type {
  AppActionResult,
  AppErrorEnvelope,
  AppMutationReceipt,
  AppRunSnapshot,
  JsonValue,
} from "@magician/apps";

export type ResultPanel =
  | { readonly kind: "pending"; readonly status: string; readonly runRef: string }
  | { readonly kind: "success"; readonly status: "completed"; readonly runRef: string; readonly output: JsonValue }
  | { readonly kind: "failure"; readonly status: "failed"; readonly runRef: string; readonly error: PublicErrorPanel }
  | { readonly kind: "uncertain"; readonly status: "uncertain" | "cancelled"; readonly runRef: string; readonly message: string };

export interface PublicErrorPanel {
  readonly code: string;
  readonly disposition: string;
  readonly message: string;
  readonly retryAfterMs?: number;
}

export interface ReceiptPanel {
  readonly receiptRef: string;
  readonly committedAt: string;
  readonly recordCount: number;
  readonly firstChange: number;
  readonly lastChange: number;
}

function renderError(error: AppErrorEnvelope | undefined): PublicErrorPanel {
  if (error === undefined) {
    return { code: "internal", disposition: "terminal", message: "The run failed without a disclosable error." };
  }
  return {
    code: error.code,
    disposition: error.disposition,
    message: error.message.slice(0, 2048),
    ...(error.retry_after_ms === undefined ? {} : { retryAfterMs: error.retry_after_ms }),
  };
}

export function renderRun(snapshot: AppRunSnapshot<JsonValue>): ResultPanel {
  if (!snapshot.terminal) return { kind: "pending", status: snapshot.status, runRef: snapshot.run_handle.run_ref };
  const result: AppActionResult<JsonValue> | undefined = snapshot.result;
  if (snapshot.status === "completed" && result?.status === "completed" && result.output !== undefined) {
    return { kind: "success", status: "completed", runRef: snapshot.run_handle.run_ref, output: result.output.value };
  }
  if (snapshot.status === "uncertain" || snapshot.status === "cancelled" || result?.status === "uncertain") {
    return {
      kind: "uncertain",
      status: snapshot.status === "cancelled" ? "cancelled" : "uncertain",
      runRef: snapshot.run_handle.run_ref,
      message: "No result is shown because terminal settlement is not proven.",
    };
  }
  return {
    kind: "failure",
    status: "failed",
    runRef: snapshot.run_handle.run_ref,
    error: renderError(result?.error),
  };
}

export function renderMutationReceipt(receipt: AppMutationReceipt): ReceiptPanel {
  return {
    receiptRef: receipt.receipt_id,
    committedAt: receipt.committed_at,
    recordCount: receipt.committed_record_revisions.length,
    firstChange: receipt.change_seq_range.first,
    lastChange: receipt.change_seq_range.last,
  };
}
