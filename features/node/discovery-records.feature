@unix
Feature: Discovery records
  A node tells others how to reach it with a signed discovery record. Other
  nodes know the node by its key, so the key stays the same from one record
  to the next.

  Scenario: A node signs its discovery record
    When the node signs a discovery record for the agent "server"
    Then the record carries its key and signature as hex
    And the record names the agent "server", the address "127.0.0.1:4242", the scope "directory" and the capability "review"

  Scenario: A node signs each record with the same key
    When the node signs a discovery record for the agent "server"
    And the node signs a discovery record for the agent "server"
    Then both records have the same endpoint and signing key
