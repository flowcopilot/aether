@unix
Feature: Directory
  A directory lets people find agents that they do not know yet. Agents
  publish signed records to the control service over TLS, and nodes search
  the directory and keep a synchronized copy of it.

  Background:
    Given a running control service
    And a signed test record and an agent key
    And the test record is published and renewed in the directory

  Scenario: A published record can be resolved and found
    Then the directory resolves "test-agent"
    And a directory search for "test" finds "test-agent"

  Scenario: A sync fetches each record only once
    When the node syncs from the directory
    Then the sync fetched 2 records
    When the node syncs from the directory
    Then the sync fetched 0 records

  Scenario: The directory keeps its address and records after a restart
    When the node syncs from the directory
    And the control service restarts on the same port
    And the node syncs from the directory
    Then the sync fetched 0 records
    And the control service stops cleanly

  Scenario: A revoked record expires on the next sync
    When the node syncs from the directory
    And the control service restarts on the same port
    And I revoke the test record in the directory
    And the node syncs from the directory
    Then the node knows the record "test-scope:test-agent" as expired
    And the control service stops cleanly
