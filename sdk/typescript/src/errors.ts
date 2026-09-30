import type { AppPublicOperationId, SupportedPublicErrorReason } from "./generated/public-contract.js";
import type { AppErrorCode, AppErrorDisposition, AppName, JsonValue } from "./types.js";

export type MagicianAppsErrorKind =
  | "cancelled"
  | "contract_mismatch"
  | "decode"
  | "http"
  | "limit_exceeded"
  | "outcome_uncertain"
  | "timeout"
  | "transport";

export type MagicianAppsRetryDisposition = "retry_identical_input";

export interface MagicianAppsErrorJson {
  readonly kind: MagicianAppsErrorKind;
  readonly operation_id: AppPublicOperationId;
  readonly message: string;
  readonly http_status?: number;
  readonly error_code?: AppErrorCode;
  readonly error_disposition?: AppErrorDisposition;
  readonly error_details?: Readonly<Record<AppName, JsonValue>>;
  readonly retry_after_ms?: number;
  readonly reason_code?: SupportedPublicErrorReason;
  readonly retry_disposition?: MagicianAppsRetryDisposition;
}

export class MagicianAppsError extends Error {
  readonly kind: MagicianAppsErrorKind;
  readonly operationId: AppPublicOperationId;
  readonly httpStatus: number | undefined;
  readonly errorCode: AppErrorCode | undefined;
  readonly errorDisposition: AppErrorDisposition | undefined;
  readonly errorDetails: Readonly<Record<AppName, JsonValue>> | undefined;
  readonly retryAfterMs: number | undefined;
  readonly reasonCode: SupportedPublicErrorReason | undefined;
  readonly retryDisposition: MagicianAppsRetryDisposition | undefined;

  constructor(
    kind: MagicianAppsErrorKind,
    operationId: AppPublicOperationId,
    message: string,
    options: {
      readonly httpStatus?: number;
      readonly errorCode?: AppErrorCode;
      readonly errorDisposition?: AppErrorDisposition;
      readonly errorDetails?: Readonly<Record<AppName, JsonValue>>;
      readonly retryAfterMs?: number;
      readonly reasonCode?: SupportedPublicErrorReason;
      readonly retryDisposition?: MagicianAppsRetryDisposition;
    } = {},
  ) {
    super(message);
    this.name = "MagicianAppsError";
    this.kind = kind;
    this.operationId = operationId;
    this.httpStatus = options.httpStatus;
    this.errorCode = options.errorCode;
    this.errorDisposition = options.errorDisposition;
    this.errorDetails = options.errorDetails;
    this.retryAfterMs = options.retryAfterMs;
    this.reasonCode = options.reasonCode;
    this.retryDisposition = options.retryDisposition;
  }

  toJSON(): MagicianAppsErrorJson {
    return {
      kind: this.kind,
      operation_id: this.operationId,
      message: this.message,
      ...(this.httpStatus === undefined ? {} : { http_status: this.httpStatus }),
      ...(this.errorCode === undefined ? {} : { error_code: this.errorCode }),
      ...(this.errorDisposition === undefined ? {} : { error_disposition: this.errorDisposition }),
      ...(this.errorDetails === undefined ? {} : { error_details: this.errorDetails }),
      ...(this.retryAfterMs === undefined ? {} : { retry_after_ms: this.retryAfterMs }),
      ...(this.reasonCode === undefined ? {} : { reason_code: this.reasonCode }),
      ...(this.retryDisposition === undefined ? {} : { retry_disposition: this.retryDisposition }),
    };
  }
}
