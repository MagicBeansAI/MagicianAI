export type WorkHelpTopic = { title: string; description: string };

// Display copy only. Route names, definition keys and saved records keep their contracts.
export const WORK_HELP = {
	all: { title: 'All work', description: 'Track tasks, scheduled checks and system activity in one place.' },
	'needs-you': { title: 'Needs you', description: 'Answer questions, review plans and unblock work waiting for your decision.' },
	goals: { title: 'Goals', description: 'Your goal workspaces: goals, the sub-goals under them and who owns each. Set one up from a template in chat.' },
	crew: { title: 'Crew', description: 'Your AI team. Give each member a role, tools and responsibilities, then see what they are working on.' },
	programs: { title: 'Playbooks', description: 'Reusable goals, instructions and success checks for your crew. Use them to guide ongoing work and track progress.' },
	quality: { title: 'Quality checks', description: 'Have agents review and improve an output against your criteria, then inspect the result before accepting it.' },
	operations: { title: 'Team routines', description: 'Control the shared schedule for company roles, start their sequence manually and inspect problems. Individual crew schedules have separate controls.' },
	tasks: { title: 'Tasks', description: 'Give an agent a specific outcome to deliver, once or on a schedule.' },
	monitors: { title: 'Monitors', description: 'Watch for changes on a schedule and notify you when a condition needs attention.' },
	internal: { title: 'System work', description: 'Background activity created by Magican, including crew cycles and supporting jobs.' },
	autopilot: { title: 'Autopilot', description: 'Let a crew member use its playbook to choose the next useful work. Permissions, budgets and approval rules still apply.' },
	cycles: { title: 'Work cycles', description: 'Scheduled check-ins where a crew member reviews its responsibilities and starts work within its task and step limits.' },
	focus: { title: 'Focus areas', description: 'A crew member’s ongoing responsibilities. Give each area a priority, instructions and an optional schedule.' },
	delegation: { title: 'Delegation', description: 'Choose which teammates this member can ask for help, and how far work can be passed along.' }
} satisfies Record<string, WorkHelpTopic>;

export function workHelpForLabel(label: string): WorkHelpTopic | undefined {
	const text = label.toLowerCase();
	if (text.includes('autopilot')) return WORK_HELP.autopilot;
	if (text.includes('team routine')) return WORK_HELP.operations;
	if (text.includes('playbook')) return WORK_HELP.programs;
	if (text.includes('focus area')) return WORK_HELP.focus;
	if (text.includes('cycle') || text.includes('scheduled check-in')) return WORK_HELP.cycles;
	if (text.includes('delegation')) return WORK_HELP.delegation;
	return Object.values(WORK_HELP).find(topic => topic.title.toLowerCase() === text);
}
