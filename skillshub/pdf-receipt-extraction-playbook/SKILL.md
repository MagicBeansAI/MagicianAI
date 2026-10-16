---
name: pdf-receipt-extraction-playbook
version: 0.1.0
description: Procedure skill — extract structured data (vendor, date, amount, line items, tax, total) from receipts, invoices, and bills. Handles text-based PDFs, scanned PDFs, photos. Pull this when the user shares a receipt/invoice/bill and asks to file, log, summarize, or expense it.
metadata:
  magician:
    skill_type: procedure
---

# PDF / Receipt / Invoice Extraction Playbook

The user hands you a file (PDF, photo, scan). You return clean structured data: who, when, how much, for what. This playbook is the decision flow + the JSON output shape + the failure-mode catalog.

## Decision flow — pick the engine based on input

Don't reach for OCR by default. The cheapest path that works is the right path.

```
1. Is it a PDF?
   → Run document-to-markdown first. It is local, fast, and preserves useful structure.
     - If output is non-empty and readable: use the Markdown directly.
     - If it reports unsupported/image-only: go to step 2.
     - If exact PDF layout, page ranges, or bounding boxes matter: use pdftotext.
     - If output is garbled because of a bad image overlay: go to step 2.

2. Is it an image (PNG/JPG) or a scanned PDF?
   → Run ocr with --engine agy. ALWAYS agy for receipts/photos.
     - Tesseract is for clean book scans, not receipts. Don't use it here.
     - For multi-page scanned PDFs, ocr handles rasterize+OCR automatically.

3. After OCR, normalize whitespace and look for the standard fields.
   → If a field is missing, return it as null. Don't hallucinate.
```

## Output JSON shape

Always return this exact shape so downstream consumers can rely on it:

```json
{
  "source_file": "<absolute path to the input>",
  "method": "pdftotext" | "ocr:agy" | "ocr:tesseract",
  "document_type": "receipt" | "invoice" | "bill" | "statement" | "other",
  "vendor": {
    "name": "<vendor name as printed>",
    "address": "<address as printed, or null>",
    "tax_id": "<GSTIN / VAT / EIN if visible, or null>"
  },
  "transaction": {
    "date": "<YYYY-MM-DD>",
    "time": "<HH:MM, 24h, or null>",
    "currency": "<ISO 4217: INR, USD, EUR, GBP, ...>",
    "subtotal": <number or null>,
    "tax": <number or null>,
    "tax_breakdown": [
      {"label": "<GST 18% / VAT / etc>", "amount": <number>}
    ],
    "tip": <number or null>,
    "discount": <number or null>,
    "total": <number>,
    "payment_method": "<cash / credit / debit / upi / null>",
    "reference_id": "<invoice number, transaction id, or null>"
  },
  "line_items": [
    {"description": "<as printed>", "quantity": <number or null>, "unit_price": <number or null>, "total": <number or null>}
  ],
  "confidence": "high" | "medium" | "low",
  "notes": "<any qualifiers, e.g., 'partial obscuring on bottom-right total — confirmed from subtotal+tax'>"
}
```

Rules:

- **Numbers are JSON numbers, NOT strings.** "$12.34" → `12.34`. Currency is a separate field.
- **Dates are ISO 8601.** "13/03/2025" → `"2025-03-13"`. If the date format is ambiguous (US vs EU), use the document's locale signals (currency, language, vendor country) to disambiguate. If still unclear, flag in `notes`.
- **Missing fields are `null`, not omitted.** Consumers can detect missing vs. zero.
- **Confidence levels**:
  - `high`: every field directly readable in the source.
  - `medium`: at least one field inferred from context (e.g. total computed from subtotal+tax).
  - `low`: significant guessing required; surface in `notes`.

## Document type classification

Identify what kind of document this is. Affects which fields are expected:

| Type | Trigger phrases / features |
|---|---|
| `receipt` | "RECEIPT", line items, payment method, no due date. Usually one-page. |
| `invoice` | "INVOICE", invoice number, due date, billing address. Often has line items. |
| `bill` | Utility / service: "BILL", account number, billing period, due date. Often one big line item. |
| `statement` | "STATEMENT", multiple transactions, balance, period (e.g., credit card statement). |
| `other` | Doesn't fit. Flag in notes. |

## Currency detection rules

The currency symbol can be missing or ambiguous. Hierarchy:

1. **Explicit ISO code printed** ("INR", "USD"): use it directly.
2. **Currency symbol** ("₹", "$", "€", "£"): map to most-likely ISO based on vendor locale.
3. **Vendor address** ("Bangalore", "Mumbai" → INR; "London" → GBP; etc.): infer.
4. **Last resort**: language of the document + GSTIN/VAT format → infer locale.
5. **Still unclear**: set `currency` to null, flag in notes.

⚠️ `$` is ambiguous (USD, CAD, AUD, SGD, HKD). Never assume USD without other signals.

## Tax breakdown — India-specific

For Indian receipts (most common case for us): look for GST split into CGST + SGST + IGST. Always capture as separate entries in `tax_breakdown`. The total tax = CGST + SGST (intra-state) or IGST (inter-state). Include GSTIN if visible — it's a 15-character alphanumeric.

For other jurisdictions: VAT, sales tax, service tax. Capture as labeled entries.

## Multi-page handling

For multi-page scanned PDFs (e.g., a 5-page invoice):

- Run OCR with --concurrency 4 (default — fast).
- Concatenate the page outputs.
- The structured fields usually live on page 1 (header) and the last page (total). Line items span pages.
- If line items span pages and you can't fit them all in the response, summarize with `"line_items": [...]` truncated and a `notes` entry "line items truncated; full text available in source file".

## Failure modes — what to do

| Failure | Detection | Action |
|---|---|---|
| Rotated scan | OCR output is gibberish or 90° characters | Re-OCR with --engine agy (it auto-rotates better than tesseract); if still bad, ask the user to rotate the source |
| Partial obscuring (folded receipt) | A field is unreadable but adjacent ones are clear | Set field to null, document in `notes`, derive from others if possible (total from subtotal+tax) |
| Faded thermal receipt | OCR returns very few characters | Surface "image quality too low — please re-scan or retype" rather than guessing |
| Multi-receipt page (collage / scan of 3 receipts on one page) | Multiple "TOTAL" lines, multiple date stamps | Return an ARRAY of receipts under a top-level `"receipts": [...]` key; flag in notes |
| Handwritten amounts | OCR misreads numbers ("0" vs "8" vs "B") | Mark confidence `low`, surface the raw OCR text in `notes` so user can verify |
| Foreign language receipt | Field labels in CJK / Arabic / etc. | Gemini handles this well; pass-through original text in non-translated fields, translate vendor name to English in a `vendor.name_translated` aux field |
| Encrypted PDF | pdftotext returns "encrypted" error | Surface clearly: "PDF is password-protected — please provide password or decrypt source" |

## What to NEVER do

- **Never reformat the vendor name.** "M/s Acme Pvt Ltd" stays "M/s Acme Pvt Ltd". Don't normalize to "Acme".
- **Never guess at line items not visible.** Better to return fewer items with high confidence than more items with hallucinated detail.
- **Never silently round.** If the total is `1,234.56`, return `1234.56`, not `1235`. Tax-and-total math should reconcile to the cent.
- **Never strip the original currency symbol from `notes`.** Useful for the user to verify.
- **Never modify the source file.** Read-only. If the user wants a re-export with OCR'd text, that's a separate step.

## Auxiliary outputs

When the user wants more than just the structured data:

- **"Summarize this receipt"**: produce a one-line summary ("`Lunch at Toast & Tonic, ₹2,840 on 2025-03-13`"). Append to the JSON as `summary`.
- **"Categorize this expense"**: add `"category": "<food / travel / supplies / saas / utilities / ...>"`. Pick from a small fixed list; flag `category_confidence` low if ambiguous.
- **"Log this to my expense sheet"**: after extraction, call the `sheets` tool to append a row. Use the JSON's fields as the row; surface the row range so the user can verify.

## When to call this skill

Pull this playbook when the request involves:

- "Here's a receipt — file it / log it / expense it"
- "Read this invoice and tell me the total"
- "Pull the line items from this bill"
- "Was the amount on this receipt ₹2,840 or ₹2,480?"
- Any file path that looks like a receipt/invoice (`.pdf`, `.jpg`, `.png` with words like "receipt"/"invoice"/"bill" in filename or recent context)

If the user just asks "what does this PDF say?" without expense context, this is overkill — use plain OCR/pdftotext and return text, not structured JSON.
