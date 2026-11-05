Feature: Personal agent access
  A person can let their own agent use a service that the node offers.
  The agent gets access only after the owner of the node agrees. The owner
  sees which agent asks and for what. Each turn gives a receipt that the
  agent can check, and the owner can take the access back at any time.

  Background:
    Given a personal agent with its own signing keys
    And a node that offers code review to personal agents

  Scenario: The agent card shows where to send work
    When the personal agent reads the agent card for code review
    Then the card gives the address of the code review offer

  Scenario: A turn without consent asks for the owner's consent
    When the personal agent asks for a review without access
    Then the node says that the owner must give consent
    And the answer links to the consent page of the admin UI

  Scenario: The agent waits until the owner decides
    When the personal agent asks the owner for read-only review access
    Then the request for access is still pending

  Scenario: The consent page shows which agent asks and for what
    Given the personal agent asks the owner for read-only review access
    When the owner opens the consent page
    Then the page shows the access "Review code (read only)"
    And the page names the personal agent

  Scenario: An agent that the owner approves gets access
    Given the personal agent asks the owner for read-only review access
    When the owner allows the request on the consent page
    Then the page says "Access approved"
    And the personal agent gets read-only review access

  Scenario: An agent with access gets a turn and a receipt it can check
    Given the owner gave the personal agent read-only review access
    When the personal agent asks for a review with its access
    Then the reply is the result from Codex
    And the reply has a receipt that the node's published keys verify
    And the receipt names the personal agent and the code review offer

  Scenario: The owner can take access back
    Given the owner gave the personal agent read-only review access
    And the personal agent asked for a review with its access
    When the owner revokes the access on the sharing page
    Then the personal agent can no longer get a review with that access
    And the node stops cleanly when the operator interrupts it
