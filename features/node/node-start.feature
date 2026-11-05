Feature: Starting a node
  An operator starts a node with `aether node serve`. With no flags, the node
  keeps all its files under the home directory, so the first start works
  without setup.

  Background:
    Given a new home directory

  Scenario: The node keeps its files under the home directory
    Given the owner is logged in to Codex
    When the node starts and opens its owner page
    Then the owner page is on "https://127.0.0.1:"
    And the node database is at ".aether/host/node.sqlite" in the home directory
    And the node uses the owner's Codex login

  Scenario: A node can start with only an invite name
    When the node starts with the invite name "ana"
    Then it listens for peers

  Scenario: The node refuses an invite name with a space
    When I start the node with the invite name "a b"
    Then the command fails with exit code 2

  Scenario: A sent task that expired while the node was off is not delivered
    Given the outbox holds a sent task that expired
    When the node starts without a hub
    And the node is woken to send its outbox
    Then the sent task is marked as not delivered
