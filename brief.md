Aether - the agent-to-agent communication protocol

An infrastructure layer to let agents publish selected capabilities, discover other agents, use approved knowledge, complete a handshake, and negotiate directly.

## Purpose

- Facilitate communication between agents for knowledge sharing.
- Let agents discover other agents that can provide useful knowledge or services.
- Let one organization's agent communicate with another organization's agent.
- Support a human → agent → agent → human interaction path.
- Let specialized agents provide access to reviewed, domain-specific knowledge.
- Allow the owner of an agent to make that agent's capabilities available to others.
- Support business discovery, lead creation, meeting coordination, and negotiation between organizations.
- Reduce the time between having a product or service ready and reaching a suitable customer from 6 months to 1 week.

## Information and knowledge model

- The organization would decide what information becomes public.
- Another organization's agent would search relevant public capabilities or knowledge.
- The application layer is separated in:
  - information explicitly made public;
  - communication channels between agents.
- Public information forms connections between organizations;
- Connections can become channels through which agents work together.
- Organizations can broadcast their agents, available knowledge, or available capabilities.
- The system is intended to support progressive discovery based on what is public, and what permissions have been given.

## Communication sequence

1. An organization publishes selected information or capabilities.
2. Another agent uses its own context to search that public information.
3. The agent discovers a relevant organization, capability, or agent.
4. The agents complete a handshake.
5. The agents communicate directly.
6. They request more relevant details where permitted.
7. They negotiate the next step.
8. They can coordinate a meeting, establish a lead, or start another business interaction.
9. Humans can enter the process when their participation or approval is required.

## Negotiation

- Negotiation is intended to happen through the communication infrastructure.
- Agents can ask questions about an exposed capability or service.
- They can determine whether the capability fits the requesting organization's need.
- They can identify the relevant people for a discussion.
- They can coordinate meeting availability.
- Agents can move an interaction from discovery to a sales or partnership discussion.
- The protocol is intended to support direct peer-to-peer agent negotiation.

## Permissions and privacy

- The owner controls what information becomes public.
- Permissions can control which information the connection can access.
- The infrastructure carries communication, while the owner of an agent remains responsible if that agent discloses information.

## Scope of the protocol

- The targeted component is the communication infrastructure between agents.
- The intended design is extensible and based on connectors and plug-ins.
- The wider goal is an ecosystem in which organizations can expose agents and approved knowledge to other agents.
