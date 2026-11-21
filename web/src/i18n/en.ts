// English, the source locale: every user-visible string of the UI. Keys are grouped by the screen
// that shows them. `{name}` is filled in by the caller; `<strong>…</strong>` wraps a part the
// caller renders as an element; an object holds the plural forms of Intl.PluralRules, chosen by
// `{count}`. Other catalogs are typed against this one (see ./catalog.ts). Each message is one
// literal, never joined with `+`: a joined string loses its literal type, and with it the check of
// its placeholders.
export const en = {
  'app.title': 'Help desk',
  'language.label': 'Language',
  'language.name': 'English',

  'error.unreachable': 'The core is not reachable.',
  'error.loadFailed': 'Could not load from the core.',

  'signIn.email': 'E-mail',
  'signIn.password': 'Password',
  'signIn.submit': 'Sign in',
  'signIn.wrong': 'Wrong e-mail or password.',

  'desk.tickets': 'Tickets',
  'desk.inbox': 'Inbox',
  'desk.signOut': 'Sign out',

  'state.open': 'Open',
  'state.waiting': 'Waiting',
  'state.resolved': 'Resolved',
  'state.closed': 'Closed',

  'customer.helpDesk': '{name} (help desk)',
  'customer.choose': 'Choose a customer…',
  'customer.loading': 'Loading customers…',

  'tickets.empty': 'No tickets yet.',
  'tickets.number': '#',
  'tickets.title': 'Title',
  'tickets.customer': 'Customer',
  'tickets.requester': 'Requester',
  'tickets.assignee': 'Assignee',
  'tickets.state': 'State',
  'tickets.updated': 'Updated',

  'ticket.back': '← Tickets',
  'ticket.customer': 'Customer',
  'ticket.changeCustomer': 'Change',
  'ticket.requester': 'Requester',
  'ticket.noRequester': 'None',
  'ticket.state': 'State',
  'ticket.assignee': 'Assignee',
  'ticket.unassigned': 'Unassigned',
  'ticket.opened': 'Opened',
  'ticket.confirmClose':
    'Close ticket #{number}? Closing is final: the ticket cannot be reopened, and a new message from the requester will open a new ticket.',
  'ticket.moved': 'Ticket moved.',
  'ticket.movedWithRequester': {
    one: 'Moved {count} ticket of this requester.',
    other: 'Moved {count} tickets of this requester.',
  },
  'ticket.emptyTimeline': 'Nothing written on this ticket yet.',
  'ticket.closedNotice':
    'This ticket is closed, which is final. A new message from the requester opens a new ticket. Agents can still add internal notes.',

  'entry.note': 'Internal note',
  'entry.outgoing': 'Outgoing message',
  'entry.incoming': 'Incoming message',
  'entry.to': 'to {address}',
  'entry.sent': 'Sent {time}',
  'entry.notSent': 'Not sent',
  'entry.queued': 'Queued',
  'entry.gaveUp': 'This message was not sent: the help desk stopped trying. Last error: {error}',
  'entry.unknownError': 'unknown',
  'entry.retrying': 'Not sent yet, will try again: {error}',

  'composer.modes': 'What you are writing',
  'composer.reply': 'Reply to requester',
  'composer.note': 'Internal note',
  'composer.hintReply': 'Sent by e-mail to <strong>{email}</strong>.',
  'composer.hintClosed': 'The ticket is closed, so it gets no more replies. Only agents see a note.',
  'composer.hintNote': 'Only agents see a note. It is <strong>never sent</strong> to the requester.',
  'composer.hintNoRequester':
    'This ticket has no requester, so there is nobody to reply to. Only agents see a note.',
  'composer.replyLabel': 'Reply to the requester',
  'composer.noteLabel': 'Internal note',
  'composer.replyPlaceholder': 'Write to the requester…',
  'composer.notePlaceholder': 'Write a note for the agents…',
  'composer.then': 'Then set the ticket to',
  'composer.sendReply': 'Send reply to {email}',
  'composer.addNote': 'Add internal note',

  'move.to': 'Move this ticket to',
  'move.requesterBelongs': 'The requester {email} belongs to {customer}.',
  'move.requesterToo': 'The requester was assigned to the wrong customer: move them too',
  'move.requesterTooWithTickets': {
    one: 'The requester was assigned to the wrong customer: move them too, with their {count} other ticket',
    other: 'The requester was assigned to the wrong customer: move them too, with their {count} other tickets',
  },
  'move.requesterStays': 'Keep the requester with {customer} and remove them from this ticket',
  'move.cancel': 'Cancel',
  'move.submit': 'Move ticket',

  'inbox.empty': 'The inbox is empty.',
  'inbox.noSubject': '(no subject)',
  'inbox.goToTicket': 'Go to ticket #{number}',
  'inbox.opened': 'Ticket #{number} opened from “{subject}”.',
  'inbox.attached': 'Message attached to ticket #{number}.',
  'inbox.discarded': 'Message from {sender} discarded.',
  'inbox.confirmDiscard': 'Discard the message from {sender}? It will not become part of any ticket.',
  'inbox.notATicket': 'Type the number of an existing ticket.',
  'inbox.open': 'Open ticket',
  'inbox.attach': 'Attach to ticket',
  'inbox.discard': 'Discard',
  'inbox.customer': 'Customer',
  'inbox.openSubmit': 'Open ticket',
  'inbox.ticketNumber': 'Ticket number',
  'inbox.attachSubmit': 'Attach',
  'inbox.cancel': 'Cancel',
} as const
