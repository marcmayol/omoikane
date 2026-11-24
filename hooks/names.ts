import type { Register } from 'claude-code'

// (It was the agent-names plugin; since 1.6 it comes with Omoikane.) With several sessions open at once, a subagent named after its task alone doesn't say which
// session it belongs to. This mod asks for "Project: task": it tells the model up front, in the
// tool's description, and refuses the call when the name doesn't fit.
//
// It only checks the shape. Whether the part before the colon really is the project is up to the
// model, and the description tells it what to put there.

const MAX = 60
const SHAPE = /^\S[^:\n]{0,29}: \S/u

const RULE = `Name it "Project: task": the project or request this session is working on, a colon, then the task in a few plain words, in the user's language. Use the name the user knows the project by, never a path. Under ${MAX} characters. Examples: "Billing app: review login", "Docs site: fix broken links".`

export const isValid = (name: string) => name.length <= MAX && SHAPE.test(name)

function reason (name: string) {
  const howLong = name.length > MAX ? `It has ${name.length} characters. ` : ''
  return `Omoikane: «${name}» isn't a valid subagent name. ${howLong}${RULE} Call the tool again with a new description.`
}

export const register: Register = on => {
  on('tool.describe', { tool: 'Agent' }, async ($, e, next) => {
    const r = await next(e)
    return { ...r, description: `${r.description}\n\nThe \`description\` parameter is what the user sees in the list of agents. ${RULE}` }
  })

  // A broken guard must not stop the work: if it fails, the subagent still starts.
  on('tool.call', { tool: 'Agent' }, ($, e, next) =>
    isValid(e.description) ? next(e) : { deny: reason(e.description) },
  ).catch(($, e, next) => next(e))
}
