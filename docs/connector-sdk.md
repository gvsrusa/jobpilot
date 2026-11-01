# Connector SDK

Status: stub. The Connector SDK arrives in release 0.5. Nothing here is implemented, and the shape may change.

A connector adds a job source through a plugin interface. Per the master spec (section 5), each connector must declare:

- **Supported operations**, for example search, fetch a posting, or list a company's openings.
- **Permitted automation capabilities**, meaning what the connector may do without a person: read only, prefill, or submit.
- **Rate limits**, the maximum request rate it honors.
- **Policy-review status**, whether the source's terms were reviewed, and the outcome.

Connectors must not use unauthorized scraping or automation, CAPTCHA or MFA bypass, or access-control circumvention. Autonomous submission is allowed only through connectors with verified permission and explicit user authorization.

Planned sources: Greenhouse, Lever, Ashby, RemoteOK, We Work Remotely, company career pages, Workday, Indeed, and LinkedIn through authorized or user-assisted access.
