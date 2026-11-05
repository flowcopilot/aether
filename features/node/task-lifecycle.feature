Feature: Task lifecycle
  A task that the node admitted can still end without an answer. The
  request command then tells the caller how the task ended and exits with
  an error, so a script can see that the task did not succeed.

  Background:
    Given a host node whose agent takes a second to answer
    And a caller with a signed authorization for the "direct" scope

  Scenario: The owner cancels a running task
    Given the host node is running
    And the caller's task is running
    When the owner cancels the caller's task
    Then the host node confirms the cancellation
    And the caller's running request fails with "TASK_STATE_CANCELED"
    And the host node stops cleanly

  Scenario: A result past its retention window is reported as expired
    Given the host node is running
    And the caller sent the task "expiring request" with a task id and the idempotency key "expiring-key"
    And the caller got the answer "fake Codex result"
    When the stored result passes its retention window
    And the caller retries the task
    Then the request fails with "task result has expired"
    And the host node stops cleanly

  Scenario: An execution with an unknown outcome needs operator review
    Given the host node is running
    And the caller's task is running
    When the lease of the running task expires
    And the caller retries the task
    Then the request fails with "operator review required"
    And the answer mentions "unknown_result"
    And the host node stops cleanly

  Scenario: A task larger than the context limit of the node fails
    Given the host node takes tasks of at most 1 context token
    And the host node is running
    And the caller imported the host's invitation
    When the caller sends the task "remote text"
    Then the request fails with "TASK_STATE_FAILED"
    And the host node stops cleanly
