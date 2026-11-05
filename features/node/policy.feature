Feature: Node policy
  The policy sets which work a node accepts and how much each task may use.
  The policy lives in the node database, so there is one source for it.

  Scenario: The first start writes a default policy to the node database
    Given a new home directory
    When the node starts and opens its owner page
    And the node stops
    Then the task workspace ".aether/tasks" exists in the home directory
    And the policy in the node database uses that workspace and allows 20000 context tokens
    And there is no policy file ".aether/policy.json" in the home directory

  Scenario: An old policy file is imported once and changes in the database win
    Given a policy file that allows 120 seconds for each task
    When the node loads its policy
    And the owner changes the time limit to 90 seconds in the node database
    And the policy file is replaced with text that is not a policy
    And the node loads its policy again
    Then the policy allows 90 seconds for each task
