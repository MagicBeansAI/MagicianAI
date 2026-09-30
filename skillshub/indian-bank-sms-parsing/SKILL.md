---
name: indian-bank-sms-parsing
version: 0.1.0
description: Procedure skill — extract structured transaction data from Indian bank, UPI, and wallet SMS notifications. Covers sender-code taxonomy (Kotak / HDFC / ICICI / SBI / Axis / Paytm / GPay / PhonePe), amount/UPI/reference regex hints, and merchant→category mapping. Pull this when reading bank SMS from iMessage / SMS dumps / Gmail forwards and you need to extract debit/credit transactions.
metadata:
  magician:
    skill_type: procedure
---

# Indian Bank SMS Parsing Playbook

The user wants you to extract structured financial transactions from raw SMS messages sent by Indian banks, UPI apps, and wallets. This playbook covers: how to identify which messages are transactional, how to map sender shortcodes to bank/wallet, how to extract every field, and how to categorise merchants.

Output target: rows that fit a `transactions` table with columns `tx_date, tx_time, amount, currency, tx_type, payment_method, merchant, category, account_hint, upi_id, reference, raw_text`.

## Step 1 — Identify transactional senders

Indian bank/wallet SMS arrives from short-code senders, NOT phone numbers. Discover them with a pattern-match query against the iMessage `handle.id` (or your SMS dump's equivalent sender field).

### Known sender shortcodes

| Bank / wallet | Common shortcode patterns |
|---|---|
| Kotak Mahindra | `VM-KOTAKB`, `VD-KOTAKB`, `JD-KOTAKB` |
| HDFC Bank | `VM-HDFCBK`, `JD-HDFCBK`, `AD-HDFCBK` |
| ICICI Bank | `VM-ICICIB`, `AD-ICICIB`, `JD-ICICIB` |
| State Bank of India | `VM-SBIBNK`, `JD-SBIINB`, `AD-SBIINB` |
| Axis Bank | `VM-AXISBK`, `AD-AXISBK` |
| Paytm Payments Bank | `AX-PAYTMB`, `JD-PAYTMB` |
| Paytm wallet | `VM-PAYTMI`, `JD-PAYTMM` |
| Google Pay | `JD-GPAY`, `VM-GPAYTM`, `AX-GPAY` |
| PhonePe | `VM-PHONEPE`, `JD-PHONEP` |
| Generic transactional | any of `VM-*`, `VD-*`, `JD-*`, `AD-*`, `AX-*`, `BZ-*`, `MD-*`, `BW-*` |

The two-letter prefix (`VM`, `JD`, `AD`, `AX`, etc.) is the transactional-sender carrier route; the hyphenated suffix is the brand. New banks/wallets get new suffixes — always include a fallback pattern match on the prefix when discovering.

### Discovery query (iMessage)

```sql
SELECT h.id AS sender, COUNT(*) AS msg_count
FROM message m
JOIN handle h ON m.handle_id = h.ROWID
WHERE m.is_from_me = 0
  AND (h.id LIKE '%-KOTAKB' OR h.id LIKE '%-HDFCBK' OR h.id LIKE '%-ICICIB'
       OR h.id LIKE '%-SBIBNK' OR h.id LIKE '%-SBIINB' OR h.id LIKE '%-AXISBK'
       OR h.id LIKE '%-PAYTMB' OR h.id LIKE '%-PHONEPE' OR h.id LIKE '%-GPAY'
       OR h.id LIKE 'VM-%' OR h.id LIKE 'VD-%' OR h.id LIKE 'JD-%'
       OR h.id LIKE 'AD-%' OR h.id LIKE 'AX-%' OR h.id LIKE 'BZ-%'
       OR h.id LIKE 'MD-%' OR h.id LIKE 'BW-%')
GROUP BY h.id
ORDER BY msg_count DESC;
```

Persist discovered senders to a `known_senders` table so subsequent runs skip the discovery cost:

```sql
CREATE TABLE IF NOT EXISTS known_senders (
  sender_code VARCHAR PRIMARY KEY,
  bank        VARCHAR,
  sender_type VARCHAR  -- 'bank' | 'upi' | 'wallet' | 'merchant'
);
```

## Step 2 — Filter out non-transactional messages

Not every message from a bank shortcode is a transaction. Skip:

- **OTP / verification codes** — usually contains "OTP", "code", "verification", "do not share"
- **Marketing / promo** — "offer", "loan up to", "credit card eligible", "EMI starting", "festive sale"
- **Information-only** — balance updates without movement ("balance is Rs. X"), credit limit changes, statement-ready notifications
- **Alerts without amount** — "card swiped at" with no rupee value (fraud alerts)

Heuristic: if the message has no rupee/INR amount AND no debit/credit verb, it's not a transaction.

## Step 3 — Extract fields from the SMS body

### amount

Look for any of these patterns (decimal optional, thousands-comma optional):

```
Rs.\s*([0-9,]+(?:\.[0-9]+)?)
INR\s*([0-9,]+(?:\.[0-9]+)?)
₹\s*([0-9,]+(?:\.[0-9]+)?)
Rs\s+([0-9,]+(?:\.[0-9]+)?)
```

Strip commas, parse as decimal. Default `currency = 'INR'`.

### tx_type

Lowercase the message; match against keyword sets:

- **debit**: `debited`, `spent`, `paid`, `purchase`, `purchased`, `txn`, `withdrawn`, `payment of`, `sent to`, `paid to`, `pos`
- **credit**: `credited`, `received`, `deposited`, `salary credited`, `refund of`, `reversal`, `cashback`, `recd`

If both sets match (rare; happens for transfer reversals), prefer the more recent verb.

### payment_method

Match against:

- `UPI` (UPI, VPA mention, `@ybl`, `@oksbi`, `@apl`, `@paytm`)
- `NEFT` (NEFT, neft txn)
- `IMPS` (IMPS, imps ref)
- `RTGS` (RTGS)
- `card` (POS, swiped, credit card, debit card)
- `cash` (ATM withdrawal, cash)
- `auto-debit` (mandate, SIP, recurring)

### merchant

Free-text after `to`, `at`, `from`, `for` keywords. Common Indian merchants by category — see Step 4. Strip suffixes like `private limited`, `pvt ltd`, `india`, the UPI handle, and any trailing reference number.

### category

See Step 4 below. Default to `other` when unsure (don't guess; the user can recategorise later).

### account_hint

Look for the last 4 digits of the account: `A/c X1234`, `acct ending 1234`, `card ****1234`, `xx1234`. Store just the 4 digits.

### upi_id

VPA pattern: `[A-Za-z0-9._-]+@[A-Za-z0-9]+`. Common handles: `@ybl`, `@oksbi`, `@apl`, `@paytm`, `@axl`, `@ibl`, `@upi`, `@hdfcbank`.

### reference

Transaction reference number: `Ref no. <digits>`, `UPI Ref <digits>`, `Txn ID <alphanumeric>`, `RRN <12 digits>`.

### tx_date / tx_time

If the SMS has an explicit date (`on 17-Mar-26`, `at 14:32`), use it. Otherwise fall back to the message's send timestamp.

## Step 4 — Categorise merchants

Map the extracted `merchant` to one of these categories. Match case-insensitively; allow substring matches.

| Category | Common Indian merchants |
|---|---|
| **food_delivery** | Swiggy, Zomato, EatSure, EatClub, FoodPanda |
| **dining** | restaurants by name; `restaurant`, `cafe`, `bistro` keywords |
| **shopping** | Amazon, Flipkart, Myntra, Meesho, Nykaa, Ajio, Tata CLiQ, Snapdeal |
| **groceries** | BigBasket, Blinkit, Zepto, JioMart, DMart, Instamart, Swiggy Instamart |
| **transport** | Uber, Ola, Rapido, Metro, Namma Metro, Quick Ride |
| **fuel** | Indian Oil, Bharat Petroleum, HPCL, Shell, IOCL, BPCL |
| **utilities** | electricity (BESCOM, MSEB, TNEB), water board, gas (HP / Bharat / Indane), broadband (ACT, JioFiber), mobile recharge (Jio, Airtel, Vi) |
| **rent** | `rent`, `housing`, NoBroker, RentEase |
| **salary** | `salary credited from <employer>`, `salary cr.`, payroll providers (RazorpayX, Paybooks) |
| **investment** | MF (Groww, Zerodha, Coin, Kuvera, ETMoney), SIP, stocks, FD, RD |
| **transfer** | self-transfer, P2P, `IMPS to self`, "sent to your" |
| **entertainment** | Netflix, Hotstar, Disney+ Hotstar, Spotify, Amazon Prime, YouTube Premium, BookMyShow, PVR, INOX |
| **health** | Pharmacy (Apollo, MedPlus, 1mg, Tata 1mg, NetMeds, PharmEasy), hospitals, diagnostic labs (Thyrocare, Dr Lal PathLabs) |
| **education** | course platforms (Unacademy, Byju's, upGrad, Coursera, Udemy), school fees, tuition |
| **travel** | MakeMyTrip, Goibibo, Yatra, IRCTC, Cleartrip, Booking.com, OYO, airlines |
| **subscriptions** | Apple, Google One, iCloud, GitHub, Notion, ChatGPT, anything monthly that's not entertainment |
| **other** | use when no category clearly fits — *don't guess* |

## Step 5 — Insert with idempotency

Always use `INSERT OR IGNORE` with a dedup key (source + source_ref) so re-processing the same SMS batch is safe:

```sql
INSERT OR IGNORE INTO transactions (
  source, source_ref, tx_date, tx_time,
  amount, tx_type, payment_method, merchant, category,
  account_hint, upi_id, reference, raw_text
) VALUES ('imessage', :rowid, :date, :time,
          :amount, :tx_type, :method, :merchant, :category,
          :acct, :upi, :ref, :raw);
```

The `source_ref` should be a stable per-message id (iMessage `ROWID`, Gmail message-id, etc.) so re-runs skip already-processed messages.

## Step 6 — Validate the batch

Before declaring the run complete:

- **Count check**: number of inserted rows should be ≤ messages processed (some skipped as non-transactional).
- **Spread check**: the time range covered should match what the user asked for.
- **Outlier check**: if a single message produced an amount > ₹10,00,000, surface it as a flag — it might be a parse error.
- **Currency check**: if any row is not INR, flag it. Indian bank SMS is overwhelmingly INR; non-INR usually means international/forex which has different verbs.

## Common SMS shapes

### Kotak debit (UPI)
```
Sent Rs.450.00 from Kotak Bank AC X1234 to swiggy@axl on 17-03-26.UPI Ref 612345678901.Not you,check kotak.com/fraud
```
→ `amount=450, tx_type=debit, payment_method=UPI, merchant=swiggy@axl → Swiggy, category=food_delivery, upi_id=swiggy@axl, reference=612345678901, account_hint=1234`

### HDFC credit (NEFT)
```
Update! INR 1,20,000.00 credited to HDFC Bank A/c XX1234 on 31-03-26 from MR EMPLOYER. Avbl bal:INR 1,45,200.00.
```
→ `amount=120000, tx_type=credit, payment_method=NEFT, merchant=MR EMPLOYER → category=salary, account_hint=1234`

### GPay debit
```
Rs.450 paid to Zepto via Google Pay. Ref: 901234567890. UPI ID:zepto@oksbi
```
→ `amount=450, tx_type=debit, payment_method=UPI, merchant=Zepto, category=groceries, upi_id=zepto@oksbi, reference=901234567890`

### ICICI POS
```
Acct XX1234 debited with Rs 2199.00 on 16-03-26; Info:POS PRCH AMAZON ONLINE; Ref:8901234567
```
→ `amount=2199, tx_type=debit, payment_method=card, merchant=Amazon, category=shopping, account_hint=1234, reference=8901234567`

## Failure modes

- **Mixed-case currency** — `Rs`, `rs`, `RS`, `INR`, `Inr` all valid. Normalise.
- **Lakhs/comma format** — `1,20,000` is one lakh twenty thousand. Strip commas, parse as 120000.
- **Multi-merchant strings** — sometimes the merchant field reads `AMAZON ONLINE BENGALURU IN` — strip city/country tokens.
- **No amount in message** — likely an information-only alert. Skip with status `non_transactional`.
- **Promotional with amount** — "Get loan up to Rs 5,00,000" — check for promo keywords AND no debit/credit verb before deciding it's transactional.
- **Reversed transactions** — `Rs X debited` followed by `Rs X reversed` on the same day → keep both rows, the user's running balance reflects both.

When unsure, set `category = 'other'` and `tx_type` based on the most recent verb. Don't fabricate fields the message doesn't contain.
