"use strict";

/**
 * Shared JSDoc surface for the serve static scripts. The runtime files remain
 * plain browser scripts; this file gives `tsc --checkJs` one place to learn the
 * payload shapes crossing the Python/JavaScript boundary.
 *
 * @typedef {Object} ServeTarget
 * @property {string} id
 * @property {string=} displayName
 * @property {string=} branch
 * @property {TargetIdentity} targetIdentity
 * @property {ServeAgentIdentity=} serveAgentIdentity
 * @property {string[]=} taskFilters
 * @property {TaskFilterEntry[]=} taskFilterEntries
 * @property {string=} laneFilterVersion
 * @property {TaskFilterInventory=} taskFilterInventory
 * @property {Object.<string, number>=} laneMetrics
 * @property {LaneInfo=} laneInfo
 * @property {number=} privateTaskCount
 * @property {TeamIdentity} teamIdentity
 * @property {string=} lifetime
 * @property {string[]=} pendingInboxKeys
 * @property {string=} pendingInboxRevision
 * @property {StatusLine=} statusLine
 *
 * @typedef {Object} TargetIdentity
 * @property {string} targetId
 * @property {string} worktreeName
 * @property {string} branch
 * @property {DriverIdentity} driver
 * @property {AgentIdentity} agent
 * @property {ThreadIdentity} thread
 *
 * @typedef {Object} DriverIdentity
 * @property {string} name
 * @property {string} model
 * @property {string} effort
 *
 * @typedef {Object} ServeAgentIdentity
 * @property {string} actorId
 * @property {ServeAgentDriverIdentity} driver
 * @property {ThreadIdentity} thread
 * @property {ServeAgentLaunchIdentity} launch
 *
 * @typedef {Object} ServeAgentDriverIdentity
 * @property {string} desired
 * @property {string=} actual
 * @property {string=} transcriptOwner
 *
 * @typedef {Object} ServeAgentLaunchIdentity
 * @property {ServeAgentLaunchFacts} desired
 * @property {ServeAgentLaunchFacts} actual
 *
 * @typedef {Object} ServeAgentLaunchFacts
 * @property {string=} model
 * @property {string=} effort
 * @property {string=} serviceTier
 * @property {string=} source
 *
 * @typedef {Object} AgentIdentity
 * @property {"configured"|"unconfigured"} state
 * @property {string=} name
 *
 * @typedef {Object} ThreadIdentity
 * @property {"bound"|"unbound"|"mismatch"} state
 * @property {string=} threadId
 * @property {string=} error
 *
 * @typedef {Object} TeamIdentity
 * @property {"member"|"none"} state
 * @property {string=} teamId
 * @property {number=} teamRevision
 * @property {number=} configRevision
 *
 * @typedef {Object} TaskFilterInventory
 * @property {TaskFilterRecord[]=} filters
 * @property {TaskFilterStem[]=} stems
 *
 * @typedef {Object} TaskFilterEntry
 * @property {string} project
 * @property {string} source
 *
 * @typedef {Object} TaskFilterRecord
 * @property {string} filter
 * @property {number=} open
 * @property {boolean=} assignable
 *
 * @typedef {Object} TaskFilterStem
 * @property {string} stem
 * @property {number=} open
 *
 * @typedef {Object} LaneInfo
 * @property {LaneInfoRow[]=} summaryRows
 * @property {LaneInfoMember[]=} members
 *
 * @typedef {Object} LaneInfoRow
 * @property {string} key
 * @property {string} value
 *
 * @typedef {Object} LaneInfoMember
 * @property {string} targetId
 * @property {LaneInfoRow[]=} rows
 *
 * @typedef {Object} StatusLine
 * @property {string=} error
 * @property {string=} preview
 * @property {string=} lastAssistantAt
 * @property {string=} agentProcessStatus
 * @property {string=} agentVisualStatus
 * @property {string=} activityStatus
 * @property {number=} pendingInboxCount
 * @property {string[]=} pendingInboxKeys
 * @property {string=} pendingInboxRevision
 *
 * @typedef {Object} MessageAttachment
 * @property {string=} name
 * @property {string=} url
 * @property {string=} contentType
 *
 * @typedef {Object} LaneMessage
 * @property {string} key
 * @property {string=} kind
 * @property {string=} threadId
 * @property {string=} body
 * @property {string=} displayBody
 * @property {string=} timestamp
 * @property {number=} ack_count
 * @property {MessageAttachment[]=} attachments
 *
 * @typedef {Object} ServeBranding
 * @property {string=} name
 */

/** @type {ServeBranding} */
var spiceServeBranding;
