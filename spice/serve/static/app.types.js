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
 * @property {string=} agentName
 * @property {string=} threadId
 * @property {string[]=} taskFilters
 * @property {string=} laneFilterVersion
 * @property {TaskFilterInventory=} taskFilterInventory
 * @property {Object.<string, number>=} laneMetrics
 * @property {LaneInfo=} laneInfo
 * @property {number=} privateTaskCount
 * @property {string=} teamId
 * @property {number=} teamRevision
 * @property {number=} configRevision
 * @property {string=} lifetime
 * @property {StatusLine=} statusLine
 *
 * @typedef {Object} TaskFilterInventory
 * @property {TaskFilterRecord[]=} filters
 * @property {TaskFilterStem[]=} stems
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
 * @property {number=} say_count
 * @property {MessageAttachment[]=} attachments
 */
