Feature: MCP server
  An agent manages its node through MCP. `aether mcp serve` speaks JSON-RPC
  on standard input and output.

  Scenario: An agent lists the Aether tools
    When an agent starts an MCP session and lists the tools
    Then the command succeeds
    And the reply has the field "serverInfo"
    And the reply contains "aether.change"
    And the reply contains "inputSchema"

  @unix
  Scenario: A change through MCP waits for the owner to confirm it
    Given a running owner page for a managed scope
    When an agent asks over the node's admin socket to add a workspace
    Then the command succeeds
    And the reply contains "pending local confirmation"
