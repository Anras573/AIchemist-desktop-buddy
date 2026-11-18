# Microsoft 365 / Outlook calendar setup

AIchemist Desktop Buddy reads your Microsoft calendar through Microsoft Graph. It ships without a
Microsoft client id, so you register your own (free) app in Microsoft Entra ID once and paste its
id into Settings. This takes about five minutes.

What the app gets: **read-only** access to your calendars (`Calendars.Read`), plus a refresh token
(`offline_access`) so you do not have to sign in every hour, and your name and sign-in name
(`openid profile`) so Settings can show which account is connected. It cannot change or delete
anything. The refresh token is stored only in your OS keychain (Windows Credential Manager or the
macOS login keychain).

## Before you start

You need access to a Microsoft Entra **directory (tenant)** to create the registration in:

- **Work or school account:** use your organization's directory. Your organization may not allow
  users to register apps; then ask your IT admin to do steps 1 to 4 for you, or see
  [Work and school accounts: consent](#work-and-school-accounts-consent).
- **Personal account only (outlook.com, hotmail.com, live.com):** since June 2024 apps must be
  registered inside a directory
  ([Microsoft announcement](https://learn.microsoft.com/entra/identity-platform/reference-breaking-changes#june-2024)).
  Get one for free by [signing up for Azure](https://aka.ms/signUpForAzure) (this creates a
  "Default Directory" you administer) or by joining the
  [Microsoft 365 Developer Program](https://aka.ms/joinM365DeveloperProgram). The calendar you
  connect is still your personal one; the directory only holds the app registration.

## 1. Create the app registration

Reference: [Register an application in Microsoft Entra ID](https://learn.microsoft.com/entra/identity-platform/quickstart-register-app).

1. Sign in to the [Microsoft Entra admin center](https://entra.microsoft.com).
2. If you have several directories, use the **Settings** (gear) icon in the top bar to switch to
   the one you want.
3. Go to **Entra ID** > **App registrations** and select **New registration**.
4. **Name:** anything, e.g. `AIchemist Desktop Buddy`. You see this name on the Microsoft consent
   screen.
5. **Supported account types:** pick who will sign in with this registration.

   | You want to connect | Choose | Tenant to enter in Settings |
   |---|---|---|
   | Personal accounts and/or accounts from any organization | **Any Entra ID Tenant + Personal Microsoft accounts** (older portal wording: "Accounts in any organizational directory and personal Microsoft accounts") | `common` |
   | Only personal Microsoft accounts | **Personal accounts only** | `consumers` |
   | Only accounts in your own organization | **Single tenant only** | your **Directory (tenant) ID** |
   | Accounts from any organization, no personal accounts | **Multiple Entra ID tenants** | `organizations` |

   If unsure, choose **Any Entra ID Tenant + Personal Microsoft accounts** and use `common`.
6. Leave **Redirect URI** empty for now and select **Register**.

## 2. Add the redirect URI

Reference: [How to add a redirect URI](https://learn.microsoft.com/entra/identity-platform/how-to-add-redirect-uri)
and [Desktop app registration](https://learn.microsoft.com/entra/identity-platform/scenario-desktop-app-configuration).

1. In your registration, go to **Authentication** (under **Manage**).
2. Select **Add a platform** (or **Add Redirect URI**) and choose **Mobile and desktop applications**.
3. Under **Custom redirect URIs**, enter exactly:

   ```text
   http://localhost
   ```

   No port and no path. During sign-in the app listens on a random free port on your own computer
   (`http://localhost:<port>`), and Microsoft ignores the port when it matches `localhost`
   redirect URIs ([redirect URI rules](https://learn.microsoft.com/entra/identity-platform/reply-url#localhost-exceptions)).
   Do not add other `localhost` URIs that differ only by port.
4. Select **Configure**.

## 3. Allow public client flows

The app is a *public client*: it has no client secret, and you must not create one.

1. Still under **Authentication**, find **Advanced settings** > **Allow public client flows**.
2. Set it to **Yes** and select **Save**.

The sign-in itself (authorization code with PKCE through your browser) works for any
**Mobile and desktop applications** redirect URI; the switch also marks the registration as a
public client, which keeps Microsoft from asking for a client secret.

## 4. Add the calendar permission

Reference: [Configure an app to access a web API](https://learn.microsoft.com/entra/identity-platform/quickstart-configure-app-access-web-apis)
and [Calendars.Read](https://learn.microsoft.com/graph/permissions-reference#calendarsread).

1. Go to **API permissions** (under **Manage**).
2. A new registration already lists **Microsoft Graph** > `User.Read`. The app does not need it;
   you can remove it (**...** > **Remove permission**) or leave it.
3. Select **Add a permission** > **Microsoft Graph** > **Delegated permissions**.
4. Search for `Calendars.Read`, tick **Calendars.Read**, and select **Add permissions**.
5. Optional: also add `offline_access` (under **OpenId permissions**). The app requests it at
   sign-in anyway, together with `openid` and `profile`; listing it here just makes it visible.

Do **not** add `Calendars.ReadWrite` or any **Application** permission.

## 5. Copy the ids

Go to **Overview** and copy:

- **Application (client) ID**, a GUID such as `00001111-aaaa-2222-bbbb-3333cccc4444`.
- **Directory (tenant) ID**, only if you chose **Single tenant only** in step 1.5.

These are identifiers, not secrets.

## 6. Connect in AIchemist Desktop Buddy

1. Open **Settings** (tray or menu bar icon > **Open Settings**), scroll to **Calendar accounts**
   and select **Add account…**. Choose **Microsoft 365 / Outlook**.
2. **Name:** anything, e.g. `Work`.
3. **Application (client) ID:** paste the Application (client) ID.
4. **Tenant:** `common` (the default), `consumers`, `organizations`, or your Directory (tenant) ID,
   as in the table in step 1.5. A verified domain such as `contoso.onmicrosoft.com` also works in
   place of the tenant ID.
5. Select **Add and sign in**. Your browser opens the Microsoft sign-in page. Pick the account,
   accept the permissions, and when the page says *You are signed in*, close the tab. The account
   row then shows *signed in as* your sign-in name.
6. Select **Calendars…** on the account row and tick the calendars to include.

The sign-in waits 5 minutes for the browser; after that, select **Sign in** (or **Sign in again**)
on the account row. Closing the Settings window cancels a sign-in that is still waiting.
**Test connection** checks the saved sign-in against Microsoft Graph.

## Work and school accounts: consent

When you sign in, Microsoft asks you to consent to the permissions. `Calendars.Read` is a delegated
permission that users can normally consent to themselves, but organizations often restrict user
consent ([user consent settings](https://learn.microsoft.com/entra/identity/enterprise-apps/configure-user-consent)).
If you see **Need admin approval**, either:

- ask an admin to grant consent for the organization: in the registration's **API permissions**,
  **Grant admin consent for &lt;organization&gt;**
  ([grant tenant-wide admin consent](https://learn.microsoft.com/entra/identity/enterprise-apps/grant-admin-consent)), or
- request approval through the admin consent workflow if your organization has enabled it.

If an admin later revokes the consent, or your password changes, or the refresh token expires
after a long time unused, the account row in Settings shows an authentication error. Select
**Sign in again** on the account row.

## Signing out

Removing the account in Settings (**Remove…** on its row, then **Remove**) deletes its refresh token from the keychain. To also revoke the
app's access on Microsoft's side, remove it at [myapplications.microsoft.com](https://myapplications.microsoft.com)
(work or school) or [account.live.com/consent/Manage](https://account.live.com/consent/Manage)
(personal).

## Troubleshooting

| Error (on the Microsoft page or in Settings) | Cause and fix |
|---|---|
| `AADSTS50011` / redirect URI mismatch | The redirect URI is missing or not under **Mobile and desktop applications**. Redo step 2 with exactly `http://localhost`. |
| `AADSTS7000218` / `invalid_client` | The registration expects a client secret. Redo step 3 (**Allow public client flows** = **Yes**) and do not use a **Web** platform redirect URI. |
| `AADSTS700016` / `unauthorized_client` | Wrong client ID, or the tenant in Settings does not match the supported account types (for example `consumers` with a single-tenant registration). |
| `AADSTS50020` or "account does not exist in tenant" | You signed in with an account the registration does not support. Check step 1.5 and the tenant. |
| `access_denied` | You cancelled, or declined consent. See [consent](#work-and-school-accounts-consent). |
| `invalid_grant` ... "sign in again" | The saved sign-in expired or was revoked. Select **Sign in again** on the account row. |
| Sign-in timed out | The browser did not come back within 5 minutes, or a firewall blocked `localhost`. Try again. |
