# Quest Hardware Public Demo

Status: decision recorded.

## Question

Should autonomous Meta Quest hardware control be used as spice's leading public
demo and credibility story?

## Decision

Not yet. Treat the Quest hardware story as an internal battle-testing proof
point and, at most, a short anonymized credibility sidebar until a clean-room
public demo exists.

The story is strong: a real headset is a higher-entropy target than a toy CLI
demo. If spice can steer agents through hardware setup, UI observation, command
execution, recovery, and validation on a physical Meta Quest device, that says
the loop handles messy reality. It is a better credibility signal than another
synthetic todo app.

But it should not be the leading public demo until all proprietary and platform
constraints are cleared. The current repository contains no public script,
video, sanitized task transcript, or owned demo application that proves the
story can be told without exposing private implementation detail.

## Public Constraints

The safe public version must respect at least these constraints:

- **SDK and platform terms**: Meta's SDK license governs use of Meta Platform
  Technologies SDK APIs, tools, docs, platform services, and related content
  (https://developers.meta.com/horizon/licenses/). Do not present internal,
  reverse-engineered, or policy-sensitive control paths as a public recipe.
- **User data**: Meta's Developer Data Use Policy applies to user data collected
  or processed through the Meta Horizon platform and requires clear disclosure,
  permitted use, security, deletion, and compliance obligations
  (https://developers.meta.com/horizon/policy/data-use/). A public demo should
  use no real user data, no bystander media, no account identifiers, and no
  private home/room capture unless consent and disclosure are explicit.
- **Branding**: Meta Quest brand guidance allows referential platform use but
  forbids implying partnership, sponsorship, or endorsement. It also requires
  full product names such as "Meta Quest" rather than bare "Quest" in press
  materials without prior approval
  (https://developers.meta.com/horizon/resources/publish-brand-guidelines/).
- **Safety**: Meta's safety guidance emphasizes clear indoor activity space,
  boundary use, setup prompts, warnings, and breaks
  (https://www.meta.com/quest/safety-center/). A public video must not show
  unsafe headset use or make autonomous control look like unattended physical
  risk.
- **Marketing assets**: Store-style trailers and visual assets have additional
  restrictions around representative content, hardware shown, logos, and safety
  guidance (https://developers.meta.com/horizon/resources/asset-guidelines/).

## What Can Be Said Now

Safe short form:

> spice has been battle-tested against real hardware workflows, including an
> agent-driven Meta Quest test rig. We are not publishing the hardware demo
> until it can be reproduced with public tooling, owned assets, and no private
> user or platform data.

This is useful as a credibility sidebar because it communicates real-world
stress without inviting readers to inspect proprietary details.

Avoid:

- naming private apps, partners, internal device fleets, accounts, or rooms;
- showing passthrough/home footage with identifying detail;
- publishing control scripts that rely on private APIs or policy-sensitive
  automation;
- using Meta logos in a way that implies endorsement;
- presenting the demo as a Meta partnership or certified integration.

## Leading Demo Criteria

Promote it to leading demo only when all are true:

1. The scenario uses an owned demo app or public first-party surface.
2. The control path uses public developer tools or an explained black-box
   harness that does not teach policy-sensitive bypasses.
3. The recording contains no private user data, account identifiers, room
   details, bystander media, or proprietary customer content.
4. The copy uses "Meta Quest" referentially and avoids endorsement language.
5. The video visibly follows hardware safety expectations.
6. The demo transcript can be published with inbox keys, ACKs, validation, and
   failure recovery intact.
7. A reviewer can reproduce the story from public artifacts or understand why
   the hardware step is intentionally abstracted.

## Better Public Shape

Build a clean-room demo instead of narrating the proprietary one:

- Create a tiny owned VR scene with obvious state changes.
- Drive it on a Meta Quest device using public tooling.
- Show spice steering, task routing, agent recovery, and final validation.
- Publish the transcript and a short video, not the whole device-control stack.
- Explain the point: spice closes the loop on physical, stateful systems, not
  just text files.

## Answer

The autonomous Quest hardware story has high credibility value, but it is not
ready to lead public messaging. The decision is "no, not yet" for the public
lead demo; "yes" for an internal proof point and sanitized sidebar; "yes later"
for a clean-room Meta Quest demo built from public-safe assets and tooling.
