Feature: Direct contacts
  Two people can connect without a directory. One gives the other a signed
  invitation, and the other imports it as a contact. The signature makes
  sure that nobody can change the invitation on the way.

  Background:
    Given the agent "server" makes a direct invitation

  Scenario: A direct invitation names its agent and the direct scope
    Then the invitation names the agent "server" and the scope "direct"

  Scenario: An imported contact resolves to the invitation
    When a node imports the invitation
    Then the command succeeds
    And the node resolves "server" to the same invitation

  Scenario: A node refuses a forged invitation
    Given the invitation is changed to name another endpoint
    When a node imports the invitation
    Then the command fails with exit code 2
