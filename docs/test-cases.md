# Test Cases — Server Acceptance Tests

Maps each automated test in `tests/server_acceptance_tests.rs` to its user story
in `docs/user-stories.md`.

---

## US1: Starting the server

*Acceptance: `GET /image.png` returns HTTP 200 within 5 seconds of startup.*

| Test name | Type | Status |
|---|---|---|
| `us1_server_responds_to_image_request` | integration (in-process HTTP) | ✅ pass |
| `us1_server_responds_quickly` | integration (in-process HTTP, timing) | ✅ pass |

---

## US2: Config error on startup

*Acceptance: Server exits ≠ 0. Stderr contains the config file path and error description.*

| Test name | Type | Status |
|---|---|---|
| `us2_missing_config_returns_error_with_path` | unit (config loader) | ✅ pass |
| `us2_malformed_config_returns_parse_error_with_path` | unit (config loader) | ✅ pass |
| `us2_missing_required_config_fields_fails` | unit (config loader) | ✅ pass |

---

## US3: Fetching the display image

*Acceptance: Response status 200, `Content-Type: image/png`, PNG width=800, height=480.*

| Test name | Type | Status |
|---|---|---|
| `us3_get_image_returns_200` | integration (in-process HTTP) | ✅ pass |
| `us3_get_image_content_type_is_png` | integration (in-process HTTP) | ✅ pass |
| `us3_get_image_dimensions_are_800x480` | integration (in-process HTTP, PNG parse) | ✅ pass |
| `us3_get_image_response_is_valid_png` | integration (in-process HTTP, PNG parse) | ✅ pass |
| `us3_custom_display_dimensions_reflected_in_png` | unit (render pipeline) | ⏭ ignored (Wave 3) |

> **Wave 3 note:** `us3_custom_display_dimensions_reflected_in_png` is marked
> `#[ignore]` because `render_display()` currently hardcodes 800×480 and ignores
> `DisplayConfig.width/height`. Wave-3 fix: thread display dimensions through
> `render_display()` and `render_current_state()`.

---

## US4: Data freshness — stale source triggers Unknown

*Acceptance: Weather data older than 3 hours (or absent) produces an Unknown decision.*

| Test name | Type | Status |
|---|---|---|
| `us4_absent_weather_causes_unknown_decision` | unit (evaluator) | ✅ pass |
| `us4_stale_weather_over_3h_causes_unknown` | unit (evaluator) | ✅ pass |
| `us4_fresh_weather_does_not_cause_unknown` | unit (evaluator) | ✅ pass |

---

## US5: Fixture / dev mode — offline rendering

*Acceptance: `GET /image.png` returns HTTP 200 with valid PNG. No outbound HTTP.*

| Test name | Type | Status |
|---|---|---|
| `us5_fixture_mode_returns_valid_png` | unit (render pipeline) | ✅ pass |
| `us5_fixture_mode_png_has_black_pixels` | unit (render pipeline) | ✅ pass |
| `us5_fixture_mode_dimensions_are_correct` | unit (render pipeline) | ✅ pass |
| `us5_fixture_mode_sources_build_without_network` | unit (source builder) | ✅ pass |

---

## US6: Source failure — server stays up and degrades gracefully

*Acceptance: After a source fetch failure, `GET /image.png` still returns HTTP 200.*

| Test name | Type | Status |
|---|---|---|
| `us6_no_source_data_still_returns_200` | integration (in-process HTTP) | ✅ pass |
| `us6_partial_source_data_still_returns_200` | integration (in-process HTTP) | ✅ pass |
| `us6_render_with_empty_state_does_not_panic` | unit (render pipeline) | ✅ pass |

---

## US7: Optional source disabled — missing API key

*Acceptance: Server starts and responds to `GET /image.png` with HTTP 200.*

| Test name | Type | Status |
|---|---|---|
| `us7_no_trail_config_sources_still_build` | unit (source builder) | ✅ pass |
| `us7_missing_trail_key_server_still_serves_image` | integration (in-process HTTP) | ✅ pass |
| `us7_render_without_trail_data_is_valid_png` | unit (render pipeline) | ✅ pass |

---

## US8: Multi-destination evaluation — worst-case decision shown

*Acceptance: Given destinations A (NoGo) and B (Go), hero zone encodes NoGo.*

| Test name | Type | Status |
|---|---|---|
| `us8_nogo_destination_beats_go_destination` | unit (evaluator) | ✅ pass |
| `us8_hero_zone_shows_nogo_when_any_destination_is_nogo` | unit (presentation layer) | ✅ pass |
| `us8_decision_priority_nogo_beats_unknown` | unit (evaluator) | ✅ pass |

---

## US9: Caution / near-miss threshold

*Acceptance: `max_temp_f = 85`, observed = 82°F → Caution, not Go.*

| Test name | Type | Status |
|---|---|---|
| `us9_temperature_near_max_returns_caution` | unit (evaluator) | ✅ pass |
| `us9_temperature_well_below_max_returns_go` | unit (evaluator) | ✅ pass |
| `us9_temperature_above_max_returns_nogo` | unit (evaluator) | ✅ pass |
| `us9_river_near_limit_returns_caution` | unit (evaluator) | ✅ pass |

---

## US10: No destinations configured — server still serves image

*Acceptance: `GET /image.png` returns HTTP 200 when `destinations.toml` is absent.*

| Test name | Type | Status |
|---|---|---|
| `us10_no_destinations_returns_200` | integration (in-process HTTP) | ✅ pass |
| `us10_no_destinations_returns_valid_png` | integration (in-process HTTP) | ✅ pass |
| `us10_render_current_state_no_destinations_is_valid` | unit (render pipeline) | ✅ pass |
| `us10_missing_destinations_toml_is_handled` | unit (config loader) | ✅ pass |

---

## US11: Device client mode — thin client fetches and refreshes

*Acceptance: No HTTP server bound; client fetches from `image_url` at configured interval.*

| Test name | Type | Status |
|---|---|---|
| `us11_device_config_parsed_from_toml` | unit (config loader) | ✅ pass |
| `us11_no_device_config_is_server_mode` | unit (config loader) | ✅ pass |
| `us11_device_config_default_refresh_interval` | unit (config loader) | ✅ pass |

---

## US12: Source polling — background refresh without client requests

*Acceptance: After one polling interval, `GET /image.png` reflects data no older than that interval.*

| Test name | Type | Status |
|---|---|---|
| `us12_state_update_reflected_in_next_render` | integration (in-process HTTP) | ✅ pass |
| `us12_domain_state_apply_is_reflected_immediately` | unit (render pipeline) | ✅ pass |

---

## Concurrent request safety

*Additional coverage for concurrent read/write safety (mentioned in acceptance criteria).*

| Test name | Type | Status |
|---|---|---|
| `concurrent_reads_on_shared_domain_state_do_not_panic` | concurrency (tokio tasks) | ✅ pass |
| `concurrent_write_and_read_do_not_deadlock` | concurrency (tokio tasks) | ✅ pass |

---

## Summary

| Category | Pass | Ignored | Fail |
|---|---|---|---|
| US1 — server startup | 2 | 0 | 0 |
| US2 — config error | 3 | 0 | 0 |
| US3 — display image | 4 | 1 | 0 |
| US4 — stale data | 3 | 0 | 0 |
| US5 — fixture mode | 4 | 0 | 0 |
| US6 — source failure | 3 | 0 | 0 |
| US7 — optional source | 3 | 0 | 0 |
| US8 — multi-destination | 3 | 0 | 0 |
| US9 — caution threshold | 4 | 0 | 0 |
| US10 — no destinations | 4 | 0 | 0 |
| US11 — device client | 3 | 0 | 0 |
| US12 — source polling | 2 | 0 | 0 |
| Concurrent safety | 2 | 0 | 0 |
| **Total** | **40** | **1** | **0** |
