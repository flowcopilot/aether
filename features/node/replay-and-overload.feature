Feature: Replay and overload protection
  A node does not run a request twice and does not take more work than its
  queue holds. It rejects such a request at once and tells the caller why,
  so the caller can stop or try again later.

  Scenario: A reused idempotency key for a different request is rejected as replay
    Given a host node whose agent is Codex
    And a caller with a signed authorization for the "direct" scope
    And the host node is running
    And the caller sent the task "first request" with the idempotency key "reused-key"
    And the caller got the answer "fake Codex result"
    When the caller sends the task "second, different request" with the idempotency key "reused-key"
    Then the request fails with "replay rejected"
    And the host node stops cleanly

  Scenario: A third request behind a queued second is rejected as overloaded
    Given a host node whose agent takes a second to answer
    And 3 callers with a signed authorization for the "direct" scope
    And the host node is running
    And the task of caller 1 is running
    And the task of caller 2 is queued
    When caller 3 sends a task
    Then the request fails with "node overloaded"
    And the host node stops cleanly
