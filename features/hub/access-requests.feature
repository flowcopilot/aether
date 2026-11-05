Feature: Access requests
  A person cannot invite a service agent, but can ask a listed service
  agent for access. The request is an invite in reverse: the service agent
  is its sender, and it approves the request and sends the bundle. Only the
  person who asked gets a contact. A service agent sees a bounded number of
  waiting requests and never sees requests from people it blocked.

  Background:
    Given law is a listed service agent
    And alice is a person

  Rule: A request is an invite from the service agent to the person who asks

    Scenario: Both sides see a request, and the service agent is its sender
      When alice asks law for access
      Then the hub answers 201
      And the answer is a request from law to alice, for alice as receiver, without endpoints
      And law sees the request as the sender, with the endpoint of alice
      And alice sees the request as the receiver, requested

    Scenario: An approved request is delivered, and only the person who asked gets a contact
      Given alice asked law for access
      When law approves the request for 3 days
      Then the hub answers 200
      And the answer is the request accepted for 3 days
      When law sends the bundle "aether-invite:x"
      Then the hub answers 200
      And alice sees the request delivered with the bundle "aether-invite:x" and the endpoint of law
      And alice has law as her only contact
      And law has no contacts

  Rule: Only the service agent approves a waiting request

    Scenario Outline: A wrong approval is refused
      Given alice asked law for access
      When <who> <action>
      Then the hub answers <status>

      Examples:
        | who        | action                           | status |
        | alice      | approves the request             | 403    |
        | a stranger | approves the request             | 404    |
        | alice      | accepts the request              | 403    |
        | law        | approves the request for 31 days | 400    |
        | law        | approves the request with a text | 400    |

    Scenario: The service agent approves a request once
      Given alice asked law for access
      When law approves the request
      Then the hub answers 200
      When law approves the request
      Then the hub answers 403

  Rule: A person asks a listed service agent by its full name

    Scenario Outline: A request to something that is not a listed service agent is not found
      When alice asks <target> for access
      Then the hub answers <status>

      Examples:
        | target                                | status |
        | a person                              | 404    |
        | an unlisted service agent             | 404    |
        | a service agent whose listing expired | 404    |
        | an unknown name                       | 404    |
        | a name without a number               | 400    |

    Scenario: A service agent cannot ask itself
      When law asks law for access
      Then the hub answers 400

    Scenario: A request cannot carry a text
      When alice asks law for access with a text
      Then the hub answers 400

    Scenario: A request can last at most 30 days
      When alice asks law for access for 31 days
      Then the hub answers 400

    Scenario: A node without a name cannot ask
      Given carol is a node without a name
      When carol asks law for access
      Then the hub answers 403

    Scenario: A second request returns the open request without a change
      Given alice asked law for access
      And the version of law is noted
      When alice asks law for access for 30 days
      Then the hub answers 201 with the same request
      And the version of law is the same
      And law sees one request

  Rule: A person has at most 5 open requests and 10 new requests a day

    Scenario: The sixth open request is refused
      Given alice has open requests to 5 service agents
      When alice asks one more service agent for access
      Then the hub answers 409 with the error "limit"

    Scenario: The eleventh request in one day is refused
      Given it is the start of a day
      And alice asked law for access and deleted the request 10 times
      When alice asks law for access
      Then the hub answers 429 with the error "rate_limited"

  Rule: A service agent does not see requests from people it blocked

    Scenario: A request from a blocked person is hidden from the service agent
      Given law blocked alice
      And bob is a person
      When alice asks law for access
      Then the hub answers 201
      When bob asks law for access
      Then the hub answers 201
      And the answers to alice and bob have the same fields
      And law sees only the request of bob
      And alice sees the request as the receiver, requested

    Scenario: The service agent blocks the person through the request
      Given bob is a person
      And bob asked law for access
      When bob blocks through the request
      Then the hub answers 404
      When law blocks through the request
      Then the hub answers 204
      And bob sees no requests
      When bob asks law for access
      Then the hub answers 201
      And law sees no request from bob

  Rule: A service agent sees 50 waiting requests, from the oldest accounts first

    Scenario: The newest accounts wait out of view
      Given 52 people with accounts of different ages asked law for access
      Then law sees 50 requests
      And law sees the request of the oldest account
      And law does not see the request of the newest account
      And the newest account sees its request as requested

  Rule: A request expires after 7 days like an invite

    Scenario: An expired request is gone and a new request is a new row
      Given it is 7 days ago
      And alice asked law for access
      When it is now
      Then law sees no requests
      And alice sees no requests
      When law approves the request
      Then the hub answers 404
      When alice asks law for access
      Then the hub answers 201 with a new request
