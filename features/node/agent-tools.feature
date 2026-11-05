Feature: Agent tools
  A node task runs Codex with the MCP servers that the owner set up for
  the agent of the node. The owner's own Codex setup stays out, so a
  remote caller cannot use the private tools of the owner. Codex never
  stops to ask for an approval that nobody can give.

  Scenario: A node task sees the MCP servers of the agent and not those of the owner
    Given a host node whose Codex answers with the MCP servers it sees
    And the agent of the node has the MCP server "agent_probe"
    And the owner's own Codex has the MCP server "owner_only"
    And a caller with a signed authorization for the "direct" scope
    And the host node is running
    And the caller imported the host's invitation
    When the caller sends the task "list your tools"
    Then the caller gets the answer "mcp: agent_probe"
    And the answer does not mention "owner_only"
    And the host node stops cleanly
