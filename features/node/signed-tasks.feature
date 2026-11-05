Feature: Signed tasks
  A caller sends a task to the node of another person. The node runs the
  task only for the endpoint that its signed authorization names, so a
  stolen authorization is of no use to anybody else.

  Background:
    Given a host node whose agent is Codex
    And a caller with a signed authorization for the "direct" scope
    And the host node is running

  Scenario: An invited caller gets the answer of the host's agent
    Given the caller imported the host's invitation
    When the caller sends the task "remote text"
    Then the caller gets the answer "fake Codex result"
    And the host node stops cleanly

  Scenario: A one-line invite carries the authorization
    Given the caller imported the host's one-line invite
    When the caller sends the task "remote text" without an authorization file
    Then the caller gets the answer "fake Codex result"
    And the host node stops cleanly

  Scenario: The node rejects the caller's authorization from another endpoint
    When another endpoint sends the caller's signed task
    Then the host node answers "request rejected"
    And the host node stops cleanly

  Scenario: A rejected task makes the request command fail
    When another endpoint sends the task "remote text" with the caller's authorization
    Then the request fails with "request rejected"
    And the host node stops cleanly
