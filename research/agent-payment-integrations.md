# Agent payment integrations for Aether

Checked against primary sources on 2026-10-18.

This review builds on two earlier files. The [discovery review][prior-discovery]
put x402 after communication and said that payment must never be the
handshake for private information. The commercial review
put AP2, UCP and ACP in a commerce layer above A2A and MCP. This file does not
repeat those points. It selects rails for the two payment phases.

"UNVERIFIED" marks a claim that no primary source confirmed.

## Decision

**Phase A (one payment for each access period): use a Stripe Checkout Session
on the service operator's own Stripe account.**

1. The service node creates one Checkout Session for each hub access request.
   It sets `client_reference_id` to the hub request ID. [Stripe Checkout
   create][stripe-checkout-create]
2. The caller's human pays on the Stripe page. A human is in the loop by
   design, so the LLM cannot pay alone.
3. The node polls the session, or `GET /v1/events`, until `payment_status` is
   `paid`. The node needs no public webhook endpoint. [Stripe
   fulfilment][stripe-fulfil] [Stripe events][stripe-events]
4. Only then does the node sign the grant and give it to the hub.

Stripe is a standard sign-up in Romania. Standard EEA cards cost 1.5% + 1.00
lei. [Stripe global][stripe-global] [Stripe RO pricing][stripe-ro-pricing]
Refunds and disputes use the normal card process. The operator is the
merchant of record. The hub never touches funds.

**Phase B (one payment for each task): use x402 v2, scheme `exact`, with EURC
or USDC on Base.**

1. The service node sends `PAYMENT-REQUIRED` at delivery time, not at queue
   time.
2. The caller signs one EIP-3009 authorization then. The outbox stores the
   signed bytes before it sends them.
3. Each retry sends the same bytes with the same `payment-identifier` (the
   task idempotency key). The token contract rejects a second use of the
   nonce, so a retry cannot pay twice. [EIP-3009][eip3009] [x402
   payment-identifier][x402-payid]
4. The service node verifies and settles in-process with the `x402-rs` crates,
   or through the non-custodial Coinbase CDP facilitator. [x402-rs][x402-rs]
   [CDP facilitator][cdp-facilitator]

Card rails do not fit Phase B. The Stripe card minimum is 0.50 USD, the fixed
fee is 1.00 lei, and per-task card tokens (Shared Payment Tokens) are a
preview API. [Stripe machine payments][stripe-machine] [Stripe
SPT][stripe-spt]

Keep Lightning (L402 or MPP over Lightning) as the second Phase B candidate.
It is the most Rust-native rail, but it needs channel liquidity and uses BTC
rather than a euro token. Do not adopt AP2, Visa, Mastercard, PayPal, Skyfire,
or Nevermined as a rail. Copy the AP2 open-mandate model for caller-side
budget rules.

Put every rail behind one `Rail` trait (see
[Rail-neutral design](#rail-neutral-design-for-aether)). Stripe and x402 must
be two implementations of it, not two code paths.

## Comparison

| Rail | Money | Who holds keys or funds | Settlement | Fees | Rust | EU and RO today | Phase A | Phase B |
| --- | --- | --- | --- | --- | --- | --- | --- | --- |
| Stripe Checkout / Payment Links | Cards, wallets, local methods | Operator's Stripe account; Stripe is the PSP | Card capture now; payout T+3 business days in RO | 1.5% + 1.00 lei standard EEA card | `async-stripe` (community) | GA in RO | **Adopt** | No: minimum and fixed fee |
| Stripe Shared Payment Tokens | Cards through an agent token | Agent's Stripe account issues; seller charges | Charge at delivery | Card fees | No crate support found | Preview; RO listed | No | Watch |
| Stripe machine payments (MPP, x402) | USDC on Tempo, Base, Solana | Stripe balance, fiat settlement | On-chain, then Stripe balance | Not published | No | US; others by request | No | Watch |
| x402 v2 `exact` | Any EIP-3009 token: USDC, EURC | Payer's wallet; payee address; facilitator is non-custodial | Base block about 2 s | CDP: 1,000 free a month, then 0.001 USD | `x402-rs` 2.0.2 (community) | Coinbase Luxembourg is a MiCA CASP for RO | Second rail | **Adopt** |
| x402 `auth-capture` / `batch-settlement` | Same | Escrow or channel contract | Authorize now, capture later | Gas | Partial | Same | No | When refunds matter |
| A2A x402 extension | x402 inside A2A tasks | Same as x402 | Same | Same | No | n/a | No | Use the x402 v2 A2A transport instead |
| AP2 v0.2 | No rail; mandates only | User signs open mandates; agent signs closed ones | Rail-dependent | None | No | FIDO standard work | No | Copy the mandate model |
| Visa Intelligent Commerce / TAP | Cards | Issuer and network tokens | Card | Interchange | `web-bot-auth` for signatures | Live in Europe through issuers; agents are pilot | No | No |
| Mastercard Agent Pay / AP4M | Cards, accounts, stablecoins | Issuer tokens | Card | Interchange | No | First EU payment 2026; partner-only | No | No |
| PayPal agent toolkit | PayPal balance, cards | PayPal on both sides | PayPal | PayPal merchant fees | No | PayPal works in RO; Agent Ready list unverified | Possible | No |
| Lightning L402 / MPP-Lightning | BTC | Each node's own keys | HTLC settles in seconds | Routing fees only | `ldk-node` 0.7.1, `mpp` 0.15.1 | No licence for own receipts (inference) | Possible | **Second candidate** |
| Skyfire | USDC, cards | Skyfire-managed wallets | Up to about 51 h | No fee now | No | No EU licence found | No | No |
| Nevermined | Cards, USDC, EURC | Smart accounts; custodial org wallets | Rail-dependent | 1% stablecoin, 2% card | No | No EU licence found | No | No |
| Circle Gateway / Nanopayments | USDC | Non-custodial unified balance | Batched on-chain | Not published | No | Circle France is EMI and CASP | No | Watch |
| Crossmint agent wallets | USDC, cards | Scoped signer on the user's wallet | On-chain | Not published | No | Crossmint Europe is CASP and PI | Caller wallet option | Caller wallet option |
| Cloudflare pay-per-crawl, Monetization Gateway, NET Dollar | USDC on Base | Cloudflare is merchant of record | Batched | Not published | No | Closed beta; US sellers | No | No |
| Catena ACK | Receipts as VCs | Rail-dependent | Rail-dependent | None | No | No | Copy the receipt pattern | Copy the receipt pattern |

## x402

**Version and governance.** x402 v2 is current. Its version history dates v2.0
to 2025-12-09. [x402 v2 spec][x402-v2] The Linux Foundation launched the x402
Foundation on 2026-04-02, and it started operations on 2026-07-14. [LF launch][lf-x402]
[LF operational][lf-x402-op] Coinbase, Cloudflare and Stripe hold the three
Technical Steering Committee seats. [x402 TSC][x402-tsc] `coinbase/x402` is now
a development fork. [coinbase/x402][coinbase-x402]

**How it works.** The server answers `402` with `PAYMENT-REQUIRED`. The client
retries with `PAYMENT-SIGNATURE`. The server replies with `PAYMENT-RESPONSE`.
Each header holds base64 JSON. v1 used `X-PAYMENT`. [x402 HTTP
transport][x402-http] The default flow is verify, then run the resource, then
settle. A facilitator has `POST /verify`, `POST /settle` and `GET /supported`.
`settlement_pending` must carry the broadcast transaction hash, so the caller
can reconcile before it retries. [x402 v2 spec][x402-v2]

**Schemes.** [x402 schemes][x402-schemes]

- `exact`: one fixed amount. It has no refund.
- `upto`: the client authorizes a maximum; the server settles the real amount.
  On EVM it needs Permit2. [upto EVM][x402-upto]
- `batch-settlement`: payment channels with cumulative vouchers, cooperative
  refund, and a timed withdraw of 15 minutes to 30 days. [batch EVM][x402-batch]
- `auth-capture` (2026-08-18): authorize, capture, void, refund and reclaim,
  with `captureDeadline` and `refundDeadline`. Its use cases include delayed
  delivery. [auth-capture][x402-authcap]

**Idempotency.** The `payment-identifier` extension carries an `id` of 16 to
128 characters. The same `id` with the same payload returns the cached
response. The same `id` with a different payload returns `409`. The client
reuses the `id` on retries. [x402 payment-identifier][x402-payid]

**Replay.** EIP-3009 binds each authorization to a random 32-byte nonce for
each payer. The contract records the nonce and reverts a second use.
`validAfter` and `validBefore` bound the window, and the EIP sets no maximum.
EIP-3009 is still a Draft. [EIP-3009][eip3009] The reference client sets
`validBefore` to now plus `maxTimeoutSeconds`. [x402 EIP-3009
client][x402-eip3009-client]

**Transports.** v2 defines HTTP, MCP and A2A transports. The A2A transport puts
payment state in task message metadata (`x402.payment.status`,
`x402.payment.required`, `x402.payment.payload`, `x402.payment.receipts`).
[x402 A2A transport][x402-a2a] The older Google [a2a-x402][a2a-x402] extension
has one release (v0.1.0, 2025-09-16), a Python library only, and examples that
still use `"x402Version": 1`. Use the Foundation's A2A transport.

**Facilitators and custody.** The facilitator "does not hold funds or act as a
custodian". [x402 facilitator][x402-facilitator] The CDP facilitator runs on
Base, Polygon, Arbitrum, Monad, World and Solana. It accepts any ERC-20 with
EIP-3009 or Permit2 on its EVM networks. The first 1,000 on-chain transactions
each month are free, then 0.001 USD each; verification is free. It runs OFAC
and KYT checks. [CDP facilitator][cdp-facilitator] [CDP networks][cdp-networks]
Anyone can run a facilitator or self-facilitate. Self-facilitation means the
server must detect duplicates itself. [x402 facilitators][x402-facilitators]
Circle also runs an x402 facilitator on Arc, Base and Polygon. [Circle
facilitator][circle-x402]

**EURC.** The EURC contract on Base answers `authorizationState(address,bytes32)`,
so it supports EIP-3009. This was checked with a direct `eth_call` on
2026-10-18. [EURC addresses][eurc-addresses] The SDK default-asset tables are
USD only, so a EUR price needs an explicit asset amount. [x402 default
assets][x402-assets] That the CDP facilitator settles EURC in practice is
UNVERIFIED.

**Settlement time.** Base gives a preconfirmation in about 200 ms, an L2 block
in about 2 s, and L1 finality in about 20 minutes. [Base finality][base-finality]

**Rust.** There is no official Rust x402 SDK. The official repository has
TypeScript, Python, Go and Java. [x402 repo][x402-repo] The community
[`x402-rs`][x402-rs] project publishes `x402-types`, `x402-chain-eip155`,
`x402-axum`, `x402-reqwest` and `x402-facilitator-local`, all at 2.0.2
(2026-07-13). It supports v1 and v2, EVM `exact` and `upto`, and Solana.
[x402-types on crates.io][crate-x402-types] The `x402` crate name is a 2025
placeholder. Coinbase publishes an official Rust `cdp-sdk` 0.9.0 for CDP
accounts and policies. [cdp-sdk Rust][cdp-sdk-rust]

**Coinbase wallets.** CDP Server Wallet v2 signs inside AWS Nitro Enclaves, and
keys never leave the enclave in plaintext. [CDP wallet security][cdp-security]
The Agentic Wallet keeps keys in Coinbase infrastructure, with session and
per-transaction limits, on Base, Polygon and Solana. Its MCP server is Beta.
[CDP Agentic Wallet][cdp-agentic] The Policy Engine and Spend Permissions
details are UNVERIFIED.

**Fit for Aether.**

- Hub out of the money path: yes. The service node verifies and settles.
- P2P: yes. The A2A transport runs inside the existing A2A-over-Iroh exchange.
- Grant only after payment: yes. Use the `upfront` order (settle, then
  release) for Phase A grants.
- Idempotent retries: yes, if the outbox resends stored bytes and never signs
  again with a new nonce while the old one can still settle.
- 7-day wait: no conflict. The caller signs at delivery with a short window.
- Refunds: `exact` has none. A refund is a new transfer from the operator.
  Use `auth-capture` when delivery can fail after payment.

## AP2

AP2 v0.2.0 shipped on 2026-04-28 and focuses on Human Not Present flows.
[AP2 releases][ap2-releases] Google gave AP2 to the FIDO Alliance the same
day. [FIDO AP2][fido-ap2]

**How it works.** v0.2 has two mandate types, Checkout Mandate and Payment
Mandate. Each has an open form (constraints) and a closed form (one
transaction). Mandates are SD-JWT verifiable credentials. In Human Not Present
mode, the user signs open mandates that hold the agent key in `cnf`. The agent
then signs closed mandates with that key. [AP2 spec][ap2-spec]

Payment Mandate constraints include `amount_range`, `budget`,
`agent_recurrence`, `allowed_payees`, `allowed_payment_instruments` and
`execution_date`. The `budget` rule adds the new amount to all earlier closed
mandates and compares the sum with `max`. An agent "MUST NOT present
subsequent open" mandates without a rejection receipt for the previous one.
The spec recommends the smallest practical `exp` for open mandates.
[AP2 spec][ap2-spec] [AP2 payment mandate][ap2-payment-mandate]

**Keys and money.** The user or a trusted surface signs open mandates. The
Credential Provider holds the payment credential. AP2 moves no money and has
no fee. x402 appears only in samples. [AP2 spec][ap2-spec] [AP2 FAQ][ap2-faq]

**SDK.** The official SDK is Python only. No Rust crate exists. [AP2 code][ap2-code]

**Fit.** AP2 is not a rail. Its open mandate is the best published model for
the caller's spending rules: a budget, allowed payees, an amount range, and a
human-present step above a limit. Copy the fields, not the SD-JWT stack.
Google UCP checkout in AI Mode and Gemini is for select merchants in the US,
Canada and Australia, so it does not apply. [UCP merchants][ucp-merchants]

## Stripe

**Checkout Sessions.** `client_reference_id` (up to 200 characters) and
`metadata` carry the request ID. `expires_at` is 30 minutes to 24 hours.
[Stripe Checkout create][stripe-checkout-create] Stripe says webhooks are
required for reliable fulfilment, because the customer may not come back to
the landing page. Fulfilment code retrieves the session and checks
`payment_status`, and it must be idempotent. [Stripe fulfilment][stripe-fulfil]
A node behind NAT can poll the session or list events for up to 30 days back.
[Stripe events][stripe-events]

**Idempotency.** Stripe stores the first response to a key. Keys are up to 255
characters. Stripe can prune a key after 24 hours. [Stripe
idempotency][stripe-idem] A 7-day outbox must therefore keep its own state and
must not trust the Stripe key window.

**Refunds, disputes, holds.** Refunds can be partial, and Stripe keeps its
original fee. [Stripe refunds][stripe-refunds] Cardholders can usually dispute
within 120 days. The dispute fee in Romania is 100 lei. [Stripe
disputes][stripe-disputes] [Stripe RO pricing][stripe-ro-pricing] A card hold
lasts 7 days, but a Visa merchant-initiated hold lasts about 4 days 18 hours.
Extended authorization reaches about 30 days on IC+ pricing only. [Stripe
holds][stripe-holds] [Stripe extended auth][stripe-extended-auth] Do not hold
a card for a 7-day task wait.

**Shared Payment Tokens.** The agent's Stripe account issues a token scoped to
one seller, one currency, a maximum amount and an expiry. The seller charges it
with a PaymentIntent. The token deactivates after use. The API version is
`2026-09-30.preview`, and Romania is listed. [Stripe SPT][stripe-spt] The
maximum `expires_at` is UNVERIFIED.

**Agentic Commerce Protocol.** ACP is a beta, date-versioned checkout protocol;
the latest stable version is 2026-04-17. Its delegated payment token is
limited by an allowance with `reason: one_time`, `max_amount` and `expires_at`,
and it requires an `Idempotency-Key`. [ACP repo][acp-repo] [ACP delegate
payment][acp-delegate] ACP targets catalog retail. Reports that ChatGPT Instant
Checkout was retired in March 2026 are UNVERIFIED. ACP does not fit API or
service access.

**Agent toolkit and MCP.** `stripe/agent-toolkit` now redirects to
`stripe/ai`. The packages are TypeScript and Python. [Stripe AI repo][stripe-ai]
The remote MCP server asks for human confirmation of writes such as refunds.
From December 2026 it accepts only keys with the Agent tag. [Stripe MCP][stripe-mcp]

**Stablecoins and x402.** Machine payments accept MPP on Tempo and Solana and
x402 on Base, all in USDC, and settle in fiat to the Stripe balance. They are
available in the US except New York; other countries must request access.
[Stripe machine payments][stripe-machine] Stripe's x402 support settles through
the CDP facilitator to a Stripe deposit address. [Stripe x402][stripe-x402]
Stablecoin payments are in private preview in the EU. They have no disputes and
no manual capture. [Stripe stablecoin payments][stripe-stablecoin]

**Connect.** With direct charges, money lands in the connected account. With
destination charges, it lands on the platform. [Stripe Connect
charges][stripe-connect] Connect makes the Aether company a platform with
onboarding and risk duties. Do not use it. Each operator uses its own account.

**Rust.** Stripe has no official Rust SDK. [Stripe SDKs][stripe-sdks]
`async-stripe` is community-run, generated from the OpenAPI spec, and active.
The stable release is 0.41.0; 1.0.0-rc.9 shipped on 2026-09-11.
[async-stripe][async-stripe] Preview APIs such as SPT probably need raw HTTP
(UNVERIFIED).

**Romania.** Payouts start after 7 days, then T+3 business days, in RON by
default. [Stripe payouts][stripe-payouts]

## Visa Intelligent Commerce and Trusted Agent Protocol

**How it works.** TAP signs agent HTTP requests with RFC 9421 message
signatures. Tags separate browsing (`agent-browser-auth`) from checkout
(`agent-payer-auth`). The merchant gets the agent key from a registry by
`keyId`. [Visa TAP][visa-tap] TAP builds on Web Bot Auth, with Cloudflare.
[Cloudflare payments release][cf-payments] The TAP repository was last updated
on 2025-10-28, and its licence needs Visa product terms. [Visa TAP][visa-tap]

**Status.** The MCP server and the Acceptance Agent Toolkit are in pilot.
[Visa MCP][visa-mcp] Intelligent Commerce Connect is in pilot with select
partners. [Visa ICC][visa-icc] On 2026-07-02 Visa reported live agentic
transactions in Europe with more than 30 issuers, including Banca Transilvania.
Visa Payment Passkeys secured all of them. [Visa Europe][visa-europe]

**Fit.** Visa secures the cardholder side. It is not a rail that a small
operator joins. An operator who accepts cards through Stripe gets these
transactions anyway. Agent onboarding is pilot or partner only.

## Mastercard Agent Pay

**How it works.** Agent Pay uses network tokens ("Agentic Tokens"), payment
passkeys, and an acceptance framework based on Web Bot Auth. [Mastercard
merchant guide][mc-guide] Mastercard co-wrote Verifiable Intent, a draft
SD-JWT delegation chain aligned with AP2. [Verifiable Intent][mc-vi]

**Status.** Santander made the first live European Agent Pay payment on
2026-03-02, and said it was not a commercial rollout. [Santander][santander]
Agent Pay for Machines (2026-06-10) targets machine micropayments across cards,
accounts and stablecoins, with partners such as Stripe, Coinbase and Tempo. It
gives no date, price or region. [Mastercard AP4M][mc-ap4m] A Romania rollout is
UNVERIFIED.

**Fit.** Same as Visa. It is partner-only and card-centred.

## PayPal

The [PayPal agent toolkit][paypal-toolkit] (1.11.0) wraps orders, invoices,
refunds, disputes and subscriptions in TypeScript and Python. The [PayPal MCP
server][paypal-mcp] runs locally or at `mcp.paypal.com`. Agent Ready serves
Braintree and PayPal merchants through ACP and UCP. [PayPal Agent
Ready][paypal-agent-ready] PYUSD is in about 70 markets, none in the EU.
[PYUSD markets][pyusd] No PayPal x402 support was found.

**Fit.** PayPal holds the funds on both sides and both parties need accounts.
It could serve Phase A like Stripe, but it adds nothing over Stripe and has no
Rust SDK. The Agent Ready country list is UNVERIFIED.

## Lightning L402 and MPP

**How it works.** The server answers `402` with a macaroon and a BOLT11
invoice. The client pays and sends the macaroon with the preimage. The server
checks the macaroon HMAC chain and that the preimage hashes to the payment
hash, with no database lookup. Caveats can add an expiry. [L402 spec][l402-spec]
Aperture, the Lightning Labs proxy, sets a `timeout` caveat for each service,
which maps to an access period. It also speaks MPP. [Aperture][aperture]
[Aperture config][aperture-conf]

**Custody and settlement.** Each node holds its own keys. A payment is final
when the HTLC settles. Routing nodes set their own fees. A BOLT11 invoice
expires after 1 hour by default. [BOLT11][bolt11] L402 has no refund flow.

**Idempotency.** One invoice per task gives one payment hash. `ldk-node`
refuses a second send of the same hash with `DuplicatePayment`. [ldk-node
errors][ldk-node-errors]

**Rust.** [`ldk-node`][ldk-node] 0.7.1 (2026-10-12) and `lightning` (LDK) are
mature. No official L402 crate exists; community crates such as
`l402_middleware` exist. MPP has an official Rust SDK, [`mpp`][mpp-rs] 0.15.1
(2026-10-12). MPP is an IETF draft (`draft-httpauth-payment-01`) from Tempo and
Stripe; its Lightning method uses a TypeScript SDK. [MPP overview][mpp]
[MPP Lightning][mpp-lightning] [IETF payment draft][ietf-payment]

**Fit.** It fits the P2P model best: the payee issues the invoice at delivery,
and the network enforces one payment per hash. The costs are channel
liquidity, node operations, and BTC price risk for euro prices.

## Skyfire

Skyfire issues JWT tokens: `kya` (identity), `pay`, and `kya-pay`. [Skyfire
intro][skyfire-intro] The seller charges a token through Skyfire's API, and
can charge it many times until the balance is spent. Token life is 10 seconds
to 24 hours. [Skyfire charge][skyfire-charge] Settlement takes up to about 51
hours. [Skyfire settlement][skyfire-settle] Every agent account has a wallet
that Skyfire manages. [Skyfire platform][skyfire-platform] Credits are
non-refundable and the terms use California law. [Skyfire terms][skyfire-terms]
No EU licence was found.

**Fit.** No. Skyfire is a custodian in the money path, the token life does not
cover 7 days, and the charge API shows no idempotency key.

## Nevermined

Nevermined sells credit, time-based and dynamic plans, settled over x402 or
MPP. [Nevermined models][nvm-models] Cards go through Stripe Connect; stablecoins
are USDC, EURC or other ERC-20 on Base. [Nevermined FAQ][nvm-faq]
[Nevermined stablecoins][nvm-stable] Organization wallets are custodial.
[Nevermined wallets][nvm-wallets] Merchants pay 1% on stablecoin and 2% on card.
[Nevermined pricing][nvm-pricing] SDKs are TypeScript and Python. No EU licence
was found.

**Fit.** No. It adds a fee and a platform between two nodes that can already
settle x402 directly.

## Circle

Circle Internet Financial Europe SAS is an ACPR-licensed EMI and the MiCA
issuer of USDC and EURC. [Circle MiCA][circle-mica] [ESMA EMT
register][esma-emt] The AMF also authorised it as a CASP for custody and
transfer in 2026. [ESMA CASP register][esma-casp] [Circle CASP][circle-casp]
Circle Wallets use MPC or passkeys. [Circle Wallets][circle-wallets] Gateway
keeps a non-custodial unified USDC balance with a 7-day trustless withdrawal.
[Circle Gateway][circle-gateway] Nanopayments batch EIP-3009 authorizations
from 0.000001 USD and went live on mainnet in July 2026.
[Circle Nanopayments][circle-nano] Arc mainnet launched on 2026-09-16.
[Circle Arc][circle-arc] Circle has no Rust payments SDK.

**Fit.** Circle is the issuer behind the Phase B assets, not a rail choice.
Nanopayments matter if per-task prices fall below the on-chain gas cost.

## Crossmint

Crossmint Europe, S.L. is a MiCA CASP (CNMV) and a PSD2 payment institution
(Bank of Spain). [Crossmint licences][crossmint-licence] An agent can be a
scoped signer on the user's non-custodial wallet, with a spend cap, allowed
counterparties and a time window enforced on-chain. [Crossmint agents][crossmint-agents]
It supports x402. [Crossmint x402][crossmint-x402] SDKs are TypeScript, Swift
and Kotlin.

**Fit.** Crossmint is a caller-wallet option for users who will not hold keys.
It is an EU-licensed provider, so the Aether company stays out of custody.

## Cloudflare

Pay per crawl is a closed beta, and Cloudflare is the merchant of record.
[Pay per crawl][cf-ppc] The Monetization Gateway (2026-09-30) is a closed beta
for eligible US sellers and buyers, settled in USDC on Base through the CDP
facilitator. [Monetization Gateway][cf-gateway] NET Dollar is still "coming
soon". [NET Dollar][cf-netdollar] The Agents SDK adds paid MCP tools through
x402. [Cloudflare x402][cf-x402] Cloudflare owns the Rust
[`web-bot-auth`][web-bot-auth] crate (0.7.0).

**Fit.** Not a rail for Aether. Cloudflare is a party in the money path.

## Catena Labs and ACK

ACK-Pay returns a Payment Required response with several payment options. A
Receipt Service then issues a receipt as a Verifiable Credential. The packages
are TypeScript only. [ACK][ack] The OCC gave Catena Trust Bank preliminary
conditional approval on 2026-09-18. [OCC Catena][occ-catena] No EU offer was
found.

**Fit.** Copy the receipt idea. Aether's signed grant can carry the payment
reference, so the grant is also the receipt.

## Regulation (EU/Romania)

**MiCA scope.** A crypto-asset service is provided "to clients" or "on behalf
of clients": custody, exchange, order transmission, transfer services and
others. [MiCA Art. 3(1)(15)-(26)][mica] Providers of non-custodial wallet
hardware or software are out of scope. [MiCA recital 83][mica] Only a credit
institution or an EMI can issue an e-money token, and an EMT counts as
electronic money. [MiCA Art. 48][mica]

**Assets.** USDC and EURC are EMTs from Circle Internet Financial Europe SAS.
[ESMA EMT register][esma-emt] USDT is in neither the EMT nor the ART register.
ESMA told CASPs to stop services for non-compliant stablecoins.
[ESMA stablecoin statement][esma-stablecoins] [ESMA opinion 2026][esma-unauth]
Support only USDC and EURC.

**Transitional period.** It ended on 1 July 2026. Unauthorised CASPs must stop
onboarding EU clients. [ESMA transitional end][esma-transition] Romania
notified 18 months. [ESMA grandfathering][esma-grandfather]

**Romanian authority.** ESMA lists Romania's MiCA authority as "TBA". [ESMA
NCA list][esma-nca] A Ministry of Finance draft emergency ordinance names ASF.
[MF draft OUG][mf-oug] No adopted ordinance was found on 2026-10-18
(UNVERIFIED). No CASP in the ESMA register has Romania as its home state.
[ESMA CASP register][esma-casp] The Romanian company cannot get a CASP licence
at home today.

**PSD2.** A person that is not a PSP and not excluded must not provide payment
services. [PSD2 Art. 37(1)][psd2] A technical provider is excluded if it never
possesses the funds. [PSD2 Art. 3(j)][psd2] The commercial agent exclusion
covers an agent for only one side. [PSD2 Art. 3(b)][psd2] The EBA says that a
CASP transferring or holding EMTs for clients also provides a PSD2 payment
service. [EBA no-action letter][eba-mica-psd2]

**PSD3 and PSR.** Council and Parliament agreed on 2025-11-27. [Council
PSD3][council-psd3] Coreper endorsed the texts in April 2026. [Council
8401/26][council-8401] Publication in the Official Journal was not found on
2026-10-18 (UNVERIFIED).

**SCA.** A payee-initiated transaction under a mandate needs no SCA, but
setting up the mandate remotely does. [EBA Q&A 2018_4031][eba-qa-4031] A card
payment that an agent starts for the payer is payer-initiated, so SCA or an
exemption applies.

**Transfer of Funds Regulation.** The travel rule applies when a CASP is on
either side. Above EUR 1,000 to or from a self-hosted address, the CASP must
assess who controls that address. [TFR Art. 2, 14, 16][tfr] A self-custodial
caller paying a self-custodial operator involves no CASP.

**DAC8 and AMLR.** CASPs report from 2026-01-01. [DAC8][dac8] AMLR applies
from 2027-07-10 and covers CASPs as obliged entities. [AMLR][amlr]

**VAT.** For electronically supplied B2C services, the place of supply is the
customer's residence above an EUR 10,000 EU-wide threshold. [VAT Directive
Art. 58, 59c][vat] Each operator is the supplier and handles its own VAT.

**Who needs a licence.**

| Actor | Licence | Condition |
| --- | --- | --- |
| Aether company (software and hub) | None | Never hold keys, session keys, funds or caller budgets. Never be merchant of record. Never convert or run a facilitator. |
| Operator paid by card through its own Stripe | None | Stripe is the PSP. The operator is the merchant. |
| Operator paid in USDC or EURC to its own address | None (inference) | It receives the price of its own service. Refunds, prepaid balances or conversion for others raise the risk. No ESMA Q&A confirms this. |
| Operator that self-facilitates x402 | None (inference) | It submits a payer-signed transfer to its own address. UNVERIFIED. |
| Custodial caller wallet | CASP, plus PSD2 coverage for EMTs | Use only providers in the ESMA CASP register: Coinbase Luxembourg, Circle France, Crossmint Europe. |
| x402 facilitator | UNVERIFIED | Which Coinbase entity runs CDP for EU users is UNVERIFIED. |

## Rail-neutral design for Aether

**The trait.** The payee side owns `quote`, `verify`, `settle` and `refund`.
The payer side owns `authorize`. Both sides own `status`.

```rust
/// Phase A: the hub access request ID.
/// Phase B: the task idempotency key.
pub struct PaymentRef(String);

pub struct Price {
    pub amount_minor: u64,
    pub currency: Currency, // "EUR", or a CAIP-19 asset such as EURC on Base
    pub unit: Unit,         // Period { days: 1..=30 } | Task
}

#[async_trait]
pub trait Rail: Send + Sync {
    fn id(&self) -> RailId; // "stripe-checkout", "x402-exact", "l402"

    /// Payee. Make a payment request bound to `reference`.
    async fn quote(&self, reference: &PaymentRef, price: &Price) -> Result<Quote>;

    /// Payer. Make or reload the authorization for this quote.
    /// Return the stored authorization if one exists for `reference`.
    async fn authorize(&self, quote: &Quote, approval: &Approval) -> Result<Authorization>;

    /// Payee. Check the authorization against the quote. Move no money.
    async fn verify(&self, quote: &Quote, auth: &Authorization) -> Result<Verified>;

    /// Payee. Move the money. Calling it twice for one reference
    /// returns the first receipt.
    async fn settle(&self, verified: Verified) -> Result<Receipt>;

    /// Either side. Read the state from the rail, not from local memory.
    async fn status(&self, reference: &PaymentRef) -> Result<SettlementState>;

    /// Payee. Return all or part of a settled payment.
    async fn refund(&self, receipt: &Receipt, amount_minor: u64) -> Result<RefundReceipt>;
}
```

**Payment reference.** Put the `PaymentRef` into every rail field that the
rail deduplicates on:

| Rail | Where the reference goes | What stops a second payment |
| --- | --- | --- |
| Stripe Checkout | `client_reference_id`, `metadata`, and the `Idempotency-Key` for session create | One open session for each reference, kept in the node's own store |
| x402 `exact` | `payment-identifier` `id` | The EIP-3009 nonce in the stored signed payload |
| L402 | Invoice description; one invoice for each reference | The payment hash |

**Rules for the outbox (Phase B).**

1. Do not sign when you queue a task. Sign when the service node returns its
   quote at delivery.
2. Write the signed authorization to disk before you send it.
3. On a retry, send the stored authorization. Do not sign again.
4. Sign again only after the old authorization is expired and `status` shows
   that it did not settle (for x402, `authorizationState(from, nonce)` is
   false after `validBefore`).
5. If the payee returns `settlement_pending`, reconcile with the transaction
   hash before you do anything else.
6. Keep the authorization window short (minutes). The 7-day wait happens
   before the quote, so it does not need a long window.

**Where verification happens.** Only the service node verifies and settles.
The hub carries the request ID, the approval state and the signed grant. The
hub never sees a payment payload, a card, a key or an amount it must enforce.
For Phase A, the node signs the grant only after `settle` returns a receipt.
The grant must contain the rail ID and the settlement reference (Stripe
session ID, transaction hash, or payment hash). Then the grant is also the
receipt.

**What goes on the signed listing.**

- `price`: amount in minor units, currency or asset, and unit (period in days,
  or task).
- `policy`: invite grants are free; request grants use `price`.
- `rails`: the accepted rails. For x402, give the CAIP-2 network, the asset
  contract, and the `payTo` address. For Stripe, give only the rail ID, because
  the node makes the session on request. For L402, give the node public key.
- `refund_policy`: a short text and the maximum refund window.
- `seller`: the legal name and country of the operator, for VAT and consumer
  law.

The service key signs the listing. A price change makes a new listing version.
A quote must name the listing version it uses.

**What the caller records.** One local row for each payment:

- `PaymentRef`, service Endpoint ID, and listing version.
- Rail, amount, currency, and the stored authorization bytes.
- The approval: automatic under the limit, or the human who approved and when.
- The settlement reference and the receipt.
- The state: `quoted`, `authorized`, `sent`, `settled`, `failed`, `refunded`.
- The budget month that the payment counts against.

**Caller spending rules.** Copy the AP2 open-mandate fields. [AP2
spec][ap2-spec]

- One monthly budget for all services. The new amount plus the month's settled
  and in-flight amounts must not exceed it.
- An amount above the approval limit needs a human, through a local page, not
  through the LLM.
- An allow-list of payees comes from the listings that the user accepted.
- The LLM can request a payment. Only the local policy code can call
  `authorize`.

## Open questions

1. Does Romania adopt its MiCA ordinance, and does ASF publish guidance on
   merchants that receive EMTs?
2. Does the CDP facilitator serve Romanian sellers, and which Coinbase entity
   runs it? Does it settle EURC?
3. Which Base gas does a self-facilitating node pay, and does it hold a hot
   key for gas? Measure the cost per task.
4. What is the maximum `expires_at` for a Stripe Shared Payment Token? When
   does SPT leave preview?
5. When does Stripe open stablecoin payments and machine payments to Romanian
   businesses?
6. Does a Phase A access period sold to an EU consumer give a 14-day
   withdrawal right, and how must the listing state the waiver? This needs a
   lawyer.
7. How does a Romanian operator book income received in EURC or USDC for tax?
   This needs the accountant.
8. Is `auth-capture` supported by the CDP facilitator and by `x402-rs`?

[prior-discovery]: agent-discovery-communication-protocols.md
[x402-v2]: https://github.com/x402-foundation/x402/blob/main/specs/x402-specification-v2.md
[x402-http]: https://github.com/x402-foundation/x402/blob/main/specs/transports-v2/http.md
[x402-a2a]: https://github.com/x402-foundation/x402/blob/main/specs/transports-v2/a2a.md
[x402-schemes]: https://github.com/x402-foundation/x402/tree/main/specs/schemes
[x402-upto]: https://github.com/x402-foundation/x402/blob/main/specs/schemes/upto/scheme_upto_evm.md
[x402-batch]: https://github.com/x402-foundation/x402/blob/main/specs/schemes/batch-settlement/scheme_batch_settlement_evm.md
[x402-authcap]: https://github.com/x402-foundation/x402/blob/main/specs/schemes/auth-capture/scheme_auth_capture.md
[x402-payid]: https://github.com/x402-foundation/x402/blob/main/specs/extensions/payment_identifier.md
[x402-eip3009-client]: https://github.com/x402-foundation/x402/blob/main/typescript/packages/mechanisms/evm/src/exact/client/eip3009.ts
[x402-assets]: https://github.com/x402-foundation/x402/blob/main/DEFAULT_ASSETS.md
[x402-repo]: https://github.com/x402-foundation/x402
[x402-tsc]: https://github.com/x402-foundation/x402/blob/main/TSC.md
[coinbase-x402]: https://github.com/coinbase/x402
[x402-facilitator]: https://docs.x402.org/core-concepts/facilitator
[x402-facilitators]: https://docs.x402.org/dev-tools/facilitators
[lf-x402]: https://www.linuxfoundation.org/press/linux-foundation-is-launching-the-x402-foundation-and-welcoming-the-contribution-of-the-x402-protocol
[lf-x402-op]: https://www.linuxfoundation.org/press/linux-foundation-announces-operational-launch-of-x402-foundation-to-standardize-internet-native-payments-for-ai-agents-and-applications
[a2a-x402]: https://github.com/google-agentic-commerce/a2a-x402/blob/main/spec/v0.2/spec.md
[eip3009]: https://eips.ethereum.org/EIPS/eip-3009
[cdp-facilitator]: https://docs.cdp.coinbase.com/x402/seller/facilitator
[cdp-networks]: https://docs.cdp.coinbase.com/x402/network-support
[cdp-security]: https://docs.cdp.coinbase.com/server-wallets/v2/introduction/security
[cdp-agentic]: https://docs.cdp.coinbase.com/agentic-wallet/welcome
[cdp-sdk-rust]: https://github.com/coinbase/cdp-sdk/tree/main/rust
[x402-rs]: https://github.com/x402-rs/x402-rs
[crate-x402-types]: https://crates.io/crates/x402-types
[circle-x402]: https://developers.circle.com/x402-facilitators/x402
[eurc-addresses]: https://developers.circle.com/stablecoins/eurc-contract-addresses
[base-finality]: https://docs.base.org/base-chain/network-information/transaction-finality
[ap2-releases]: https://github.com/google-agentic-commerce/AP2/releases
[ap2-spec]: https://github.com/google-agentic-commerce/AP2/blob/main/docs/ap2/specification.md
[ap2-payment-mandate]: https://github.com/google-agentic-commerce/AP2/blob/main/docs/ap2/payment_mandate.md
[ap2-faq]: https://github.com/google-agentic-commerce/AP2/blob/main/docs/faq.md
[ap2-code]: https://github.com/google-agentic-commerce/AP2/blob/main/code/README.md
[fido-ap2]: https://fidoalliance.org/google-donates-agent-payments-protocol-to-fido-alliance/
[ucp-merchants]: https://support.google.com/merchants/answer/16837055
[stripe-checkout-create]: https://docs.stripe.com/api/checkout/sessions/create
[stripe-fulfil]: https://docs.stripe.com/checkout/fulfillment
[stripe-events]: https://docs.stripe.com/api/events/list
[stripe-idem]: https://docs.stripe.com/api/idempotent_requests
[stripe-refunds]: https://docs.stripe.com/refunds
[stripe-disputes]: https://docs.stripe.com/disputes/how-disputes-work
[stripe-holds]: https://docs.stripe.com/payments/place-a-hold-on-a-payment-method
[stripe-extended-auth]: https://docs.stripe.com/payments/extended-authorization
[stripe-spt]: https://docs.stripe.com/agentic-commerce/concepts/shared-payment-tokens
[stripe-machine]: https://docs.stripe.com/payments/machine
[stripe-x402]: https://docs.stripe.com/payments/machine/x402
[stripe-stablecoin]: https://docs.stripe.com/payments/stablecoin-payments
[stripe-connect]: https://docs.stripe.com/connect/charges
[stripe-ai]: https://github.com/stripe/ai
[stripe-mcp]: https://docs.stripe.com/mcp
[stripe-sdks]: https://docs.stripe.com/sdks
[stripe-global]: https://stripe.com/global
[stripe-ro-pricing]: https://stripe.com/ro/pricing
[stripe-payouts]: https://docs.stripe.com/payouts
[async-stripe]: https://github.com/arlyon/async-stripe
[acp-repo]: https://github.com/agentic-commerce-protocol/agentic-commerce-protocol
[acp-delegate]: https://github.com/agentic-commerce-protocol/agentic-commerce-protocol/blob/main/rfcs/rfc.delegate_payment.md
[visa-tap]: https://github.com/visa/trusted-agent-protocol
[visa-mcp]: https://corporate.visa.com/en/sites/visa-perspectives/innovation/visa-mcp-server-agent-acceptance-toolkit.html
[visa-icc]: https://usa.visa.com/about-visa/newsroom/press-releases.releaseId.22276.html
[visa-europe]: https://www.visa.co.uk/about-visa/newsroom/press-releases.3457328.html
[cf-payments]: https://www.cloudflare.com/press/press-releases/2025/cloudflare-collaborates-with-leading-payments-companies-to-secure-and-enable-agentic-commerce/
[mc-guide]: https://developer.mastercard.com/merchant-cloud/documentation/tutorials-and-guides/agentic-commerce-guide/
[mc-vi]: https://github.com/agent-intent/verifiable-intent
[mc-ap4m]: https://investor.mastercard.com/investor-news/investor-news-details/2026/Mastercard-Launches-Agent-Pay-for-Machines-to-Unlock-Super-Fast-Always-On-Payments/default.aspx
[santander]: https://www.santander.com/en/press-room/press-releases/2026/03/santander-and-mastercard-complete-europes-first-live-end-to-end-payment-executed-by-an-ai-agent
[paypal-toolkit]: https://github.com/paypal/agent-toolkit
[paypal-mcp]: https://developer.paypal.com/tools/mcp-server/
[paypal-agent-ready]: https://developer.paypal.com/agent-ready/overview
[pyusd]: https://history.paypal.com/us/cshelp/article/where-is-pyusd-available-help1326
[l402-spec]: https://github.com/lightninglabs/L402/blob/master/protocol-specification.md
[aperture]: https://github.com/lightninglabs/aperture
[aperture-conf]: https://github.com/lightninglabs/aperture/blob/master/sample-conf.yaml
[bolt11]: https://github.com/lightning/bolts/blob/master/11-payment-encoding.md
[ldk-node]: https://crates.io/crates/ldk-node
[ldk-node-errors]: https://docs.rs/ldk-node/0.7.1/ldk_node/enum.NodeError.html
[mpp]: https://mpp.dev/overview
[mpp-lightning]: https://mpp.dev/payment-methods/lightning
[mpp-rs]: https://github.com/tempoxyz/mpp-rs
[ietf-payment]: https://datatracker.ietf.org/doc/draft-httpauth-payment/
[skyfire-intro]: https://docs.skyfire.xyz/docs/introduction-to-skyfire
[skyfire-charge]: https://docs.skyfire.xyz/reference/charge-token
[skyfire-settle]: https://docs.skyfire.xyz/docs/payments-settlement
[skyfire-platform]: https://docs.skyfire.xyz/docs/platform-structure
[skyfire-terms]: https://skyfire.xyz/terms-of-service/
[nvm-models]: https://nevermined.ai/docs/integrate/patterns/payment-models
[nvm-faq]: https://nevermined.ai/docs/products/payments/faq
[nvm-stable]: https://nevermined.ai/docs/integrate/patterns/stablecoin-payments
[nvm-wallets]: https://nevermined.ai/docs/solutions/organizations/wallets
[nvm-pricing]: https://nevermined.ai/pricing
[circle-mica]: https://www.circle.com/pressroom/circle-is-first-global-stablecoin-issuer-to-comply-with-mica-eus-landmark-crypto-law
[circle-casp]: https://www.circle.com/blog/circle-france-receives-approval-to-offer-crypto-asset-services-under-mica
[circle-wallets]: https://developers.circle.com/wallets
[circle-gateway]: https://developers.circle.com/gateway
[circle-nano]: https://www.circle.com/blog/nanopayments-powered-by-circle-gateway-is-now-live-on-mainnet
[circle-arc]: https://www.circle.com/pressroom/circle-launches-arc-mainnet-an-economic-operating-system-for-the-internet
[crossmint-licence]: https://www.crossmint.com/announcement/psd2-mica
[crossmint-agents]: https://docs.crossmint.com/agents/how-agents-pay
[crossmint-x402]: https://docs.crossmint.com/agents/payment-flows/x402
[cf-ppc]: https://developers.cloudflare.com/ai-crawl-control/features/pay-per-crawl/what-is-pay-per-crawl/
[cf-gateway]: https://blog.cloudflare.com/monetization-gateway-beta/
[cf-netdollar]: https://netdollar.cloudflare.com/
[cf-x402]: https://developers.cloudflare.com/agents/x402/
[web-bot-auth]: https://github.com/cloudflare/web-bot-auth
[ack]: https://github.com/agentcommercekit/ack
[occ-catena]: https://www.occ.gov/topics/charters-and-licensing/interpretations-and-decisions/2026/cd1392.pdf
[mica]: https://eur-lex.europa.eu/legal-content/EN/TXT/?uri=CELEX:32023R1114
[esma-emt]: https://www.esma.europa.eu/sites/default/files/2024-12/EMTWP.csv
[esma-casp]: https://www.esma.europa.eu/sites/default/files/2024-12/CASPS.csv
[esma-stablecoins]: https://www.esma.europa.eu/sites/default/files/2025-01/ESMA75-223375936-6099_Statement_on_stablecoins.pdf
[esma-unauth]: https://www.esma.europa.eu/press-news/esma-news/esma-sets-out-supervisory-expectations-services-related-unauthorised
[esma-transition]: https://www.esma.europa.eu/sites/default/files/2026-06/ESMA75-113276571-1710_Public_Statement_MiCA_transitional_period_ends.pdf
[esma-grandfather]: https://www.esma.europa.eu/sites/default/files/2024-12/List_of_MiCA_grandfathering_periods_art._143_3.pdf
[esma-nca]: https://www.esma.europa.eu/sites/default/files/2024-12/List_of_Competent_Authorities_notified_to_ESMA_under_MiCA.pdf
[mf-oug]: https://mfinante.gov.ro/static/10/Mfp/transparenta/NFproiectOUGMICA_23052025.pdf
[psd2]: https://eur-lex.europa.eu/legal-content/EN/TXT/?uri=CELEX:32015L2366
[eba-mica-psd2]: https://www.eba.europa.eu/publications-and-media/press-releases/eba-publishes-no-action-letter-interplay-between-payment-services-directive-psd23-and-markets-crypto
[council-psd3]: https://www.consilium.europa.eu/en/press/press-releases/2025/11/27/payment-services-council-and-parliament-agree-to-step-up-the-fight-against-fraud-and-increase-transparency/
[council-8401]: https://data.consilium.europa.eu/doc/document/ST-8401-2026-INIT/en/pdf
[eba-qa-4031]: https://www.eba.europa.eu/single-rule-book-qa/qna/view/publicId/2018_4031
[tfr]: https://eur-lex.europa.eu/legal-content/EN/TXT/?uri=CELEX:32023R1113
[dac8]: https://eur-lex.europa.eu/legal-content/EN/TXT/?uri=CELEX:32023L2226
[amlr]: https://eur-lex.europa.eu/legal-content/EN/TXT/?uri=CELEX:32024R1624
[vat]: https://eur-lex.europa.eu/legal-content/EN/TXT/?uri=CELEX:02006L0112-20250101
