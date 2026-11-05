Feature: Scopes
  A node serves callers in direct, directory and managed scopes. One node
  can serve several scopes at the same time. In a managed scope, the
  roster decides who can send work, so a revoked member loses access at
  once.

  Scenario: A member of the managed roster gets an answer
    Given a host node whose agent is Codex
    And a caller with a signed authorization for the "managed" scope
    And the caller is a member of the managed roster
    And the host node is running
    When the caller sends the task "remote text"
    Then the caller gets the answer "fake Codex result"
    And the host node stops cleanly

  Scenario: A revoked member of the managed roster is denied
    Given a host node whose agent is Codex
    And a caller with a signed authorization for the "managed" scope
    And the caller is a member of the managed roster
    And the host node is running
    And the caller sent the task "remote text"
    And the caller got the answer "fake Codex result"
    When the roster revokes the caller
    And the caller sends the task "remote text"
    Then the request fails with "permission denied"
    And the host node stops cleanly

  Scenario Outline: A node with several scopes admits a task in each scope
    Given a host node whose agent is Codex
    And a caller with a signed authorization for the "direct" scope
    And the caller also has a signed authorization for the "directory" scope
    And the caller also has a signed authorization for the "managed" scope
    And the caller is a member of the managed roster
    And the host node is running
    When the caller sends the task "remote text" in the "<scope>" scope
    Then the caller gets the answer "fake Codex result"
    And the host node stops cleanly

    Examples:
      | scope     |
      | direct    |
      | directory |
      | managed   |
