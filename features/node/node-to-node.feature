@node-to-node
Feature: Node-to-node requests
  Two nodes exchange work over an encrypted peer connection. A client finds
  the serving node in any scope and gets its answer in the same scope. A
  node serves more than one request at a time.

  Scenario Outline: Two nodes exchange a request in each scope
    Given a serving node that the client finds in the "<scope>" scope
    When the client sends a review request in the "<scope>" scope
    Then the client receives a submitted event and then a result, both in the "<scope>" scope

    Examples:
      | scope     |
      | direct    |
      | directory |
      | managed   |

  Scenario: A node serves two requests at the same time
    Given a serving node whose handler waits until two requests arrive
    When two clients send a review request at the same time
    Then both requests complete
    And the node ran both handlers at the same time
