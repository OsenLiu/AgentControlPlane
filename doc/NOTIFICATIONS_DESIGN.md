# System notifications design

## Status

Design confirmed. This document describes the notification behavior and user
controls; it does not specify implementation details beyond the existing
desktop-app context.

## Goal

Alert users through native operating-system notifications when an agent needs
their attention or a task reaches a terminal result. Notifications supplement
the app's existing attention and outcome views; they do not replace them.
Task outcomes are loaded from persisted session and task state, not notification
events. Task outcomes show completed sessions until their task is marked done
or archived. Archiving a session also removes it from Task outcomes. Failed
sessions are not shown there.

The design targets Windows, macOS, and Linux. Platform-specific authorization
and delivery requirements must be respected, and the UI must not imply that a
notification was delivered when the operating system blocks it.

## Notification events

Send one notification for each:

- **New actionable attention item**, including an agent permission/input
  request or an equivalent blocker that requires the user to respond or act.
  Routine session updates are not actionable attention.
- **Task terminal result**, whether the task succeeds or fails. Do not notify
  for individual steps or intermediate status changes.

Send notifications even when the app is visible. Do not automatically repeat
a dismissed or ignored notification. Keep separate events separate and let the
operating system group notifications where supported.

## Message content

Use concise, English-only messages, matching the current UI language. Do not
include task or session names, prompts, agent output, or other potentially
sensitive content.

| Event | Title | Body |
| --- | --- | --- |
| Actionable attention | Agent needs your attention | Open Agent Control Plane to review a pending request. |
| Successful task | Task completed | Open Agent Control Plane to view the outcome. |
| Failed task | Task failed | Open Agent Control Plane to view the outcome. |

Use the operating system's default notification sound and normal display and
dismissal behavior. There is no separate sound or duration control.

## Click behavior

- Clicking an attention notification opens the associated session and focuses
  its pending attention item. It must not approve, reject, or respond on the
  user's behalf.
- Clicking a task-result notification opens that task's outcome details.

## Settings and preferences

Add a **Notifications** section to the existing Settings page. Preferences are
global across the app and persist with the other app settings.

- Provide a master notification switch and separate **Attention** and **Task
  completion** switches.
- Enable the master and both categories by default.
- Turning off the master preserves the category selections. Turning it back
  on restores those selections.
- Turning off a category suppresses only that category.
- Do not queue or replay events that occurred while notifications were
  disabled.

## Operating-system permission

Because notifications are enabled by default, do not request operating-system
permission on first launch. At the first notification-worthy event, explain
why permission is needed and then request it.

If permission is denied or blocked:

- Preserve the user's in-app notification preferences.
- Show an explicit blocked status in Settings and provide guidance for
  enabling notifications in the operating-system settings.
- Do not claim that notifications were delivered.
- Keep attention items and task outcomes available in their existing in-app
  views.

## App lifetime and platform behavior

Deliver notifications while the app process is running, including when its
window is hidden. Do not add tray or background-service behavior as part of
this design. No notifications are expected after the app has fully quit,
because its in-process task workers stop with it.

Use native system notification presentation on Windows, macOS, and Linux,
including each platform's standard sound and dismissal behavior. Respect
platform authorization and delivery constraints; notification support must
not be represented as guaranteed when the operating system or desktop
environment blocks it.

## Acceptance criteria

- Actionable attention and successful/failed task completion produce the
  specified system notifications on supported Windows, macOS, and Linux
  environments.
- Routine updates and intermediate task steps do not generate notifications.
- Notifications use the specified generic copy, contain no task or agent
  content, and use the operating system's default sound and dismissal
  behavior.
- Notification clicks open the related attention item or Task outcomes without
  taking an action on the user's behalf. An outcome is shown only while it
  remains eligible for that view.
- The master and category switches behave independently as described and
  persist across app restarts.
- Disabled categories do not produce notifications or replay missed events.
- Permission denial is visible and actionable in Settings; in-app attention
  and outcome views remain available.
- Notifications work only while the app process is running; no tray,
  background service, or post-quit delivery is required.
