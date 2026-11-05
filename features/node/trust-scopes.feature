Feature: Trust scopes
  A caller can reach a node through a direct invitation, a public directory
  or a managed hub. Each way is a scope. The node gives the same limited
  access in each scope, and a grant in one scope does not open another.

  Scenario Outline: Each scope admits the same limited capability
    Given the caller is a known peer in the "<scope>" scope
    And the caller holds a review grant for the "<scope>" scope
    When the caller asks to review in the "<scope>" scope
    Then the "<scope>" scope lists the caller
    And the node approves the request

    Examples:
      | scope     |
      | direct    |
      | directory |
      | managed   |

  Scenario: A grant for one scope does not admit work in another scope
    Given the caller holds a review grant for the "managed" scope
    When the caller asks to review in the "directory" scope
    Then the node does not approve the request

  Scenario: Each scope keeps its own listing of the caller
    Given the caller is a known peer in the "directory" scope
    And the caller is a known peer in the "managed" scope
    Then the "directory" scope lists the caller
    And the "managed" scope lists the caller

  Scenario: The hub revokes a managed member
    Given the caller is a known peer in the "managed" scope
    When the hub revokes the caller from the managed scope
    Then the managed roster marks the caller as revoked
