/* Multi LLM – legal texts shared by the desktop app, the embedded web frontend
   and the website. One source of truth: the wording must not drift between the
   three surfaces.

   The app text differs from the website text in exactly one respect, and that
   difference is deliberate:
     - website: GitHub Pages writes server logs, so the banner asks for consent
     - app:     everything stays on the user's machine, so there is nothing to
                consent to and the app text says so plainly
   The paragraphs themselves (imprint, data processing, terms) are identical. */

export interface LegalSection {
  id: string;
  title: string;
  body: string;
}

export interface LegalDoc {
  headline: string;
  intro: string;
  sections: LegalSection[];
}

export const APP_VERSION_LABEL = "Multi LLM 1.0.5";

/* Shown in the app and in the Docker/server build. */
export const APP_LEGAL: LegalDoc = {
  headline: "Legal",
  intro:
    "Imprint, privacy policy, and terms of use for the Multi LLM desktop app and the Docker image " +
    "– in English and German.",
  sections: [
    {
      id: "imprint",
      title: "Imprint",
      body:
        "Service provider within the meaning of § 5 DDG (German Digital Services Act):\n" +
        "John Weide\n" +
        "Ernst-Barlach-Straße 53\n" +
        "79312 Emmendingen\n" +
        "Germany\n" +
        "\n" +
        "Contact:\n" +
        "Email: randompixxelstudios@gmail.com\n" +
        "\n" +
        "Responsible for content according to § 18(2) MStV:\n" +
        "John Weide, address as above\n" +
        "\n" +
        "About this project.\n" +
        "MULTILLM is a software project (currently in beta): a desktop app (Windows/Linux) and a " +
        "Docker image that route OpenAI-compatible API requests to configured LLM providers. It is a " +
        "private, non-commercial project by an individual, not a company.\n" +
        "\n" +
        "EU dispute resolution:\n" +
        "The European Commission provides a platform for online dispute resolution (ODR): " +
        "https://ec.europa.eu/consumers/odr/ – you can find our email address above.\n" +
        "\n" +
        "Consumer arbitration:\n" +
        "We are neither willing nor obliged to participate in dispute-settlement proceedings before a " +
        "consumer arbitration body."
    },
    {
      id: "privacy",
      title: "Privacy",
      body:
        "1. Your data never leaves your machine.\n" +
        "The desktop app runs entirely on your own computer. Provider API keys, prompts, responses " +
        "and usage figures are stored locally in your user profile and are never transmitted to us. " +
        "We operate no server that could receive them. Requests go from the app directly to the LLM " +
        "providers you configured, under their own terms.\n" +
        "\n" +
        "2. No tracking, no analytics, no advertising.\n" +
        "The app contains no advertising and no analytics. It sets no tracking cookies. It does not " +
        "collect telemetry of any kind.\n" +
        "\n" +
        "3. What the app stores locally.\n" +
        "Settings, provider configurations, API keys and usage statistics are written to your local " +
        "profile directory. API keys are stored separately from the rest of the configuration. " +
        "Deleting the data directory, or exporting your config without keys and deleting it, removes " +
        "them from this device.\n" +
        "\n" +
        "4. The only network connection the app makes itself.\n" +
        "If you leave “Check for updates automatically” switched on, the app fetches a small " +
        "JSON manifest once at start-up to ask whether a newer version exists. The manifest lives at " +
        "randompixelstudios.github.io/Multi-LLM/update.json, which is served by GitHub, Inc. " +
        "(88 Colin P. Kelly Jr. St., San Francisco, CA 94107, United States) and falls under the " +
        "GitHub Privacy Statement. The request transmits your IP address, and GitHub records it in " +
        "its server logs. The reply contains only a version number and a download link - never " +
        "anything about your machine, your providers or your usage. Switch the check off in " +
        "Settings → App and no such request is made. You can also point the app at a different " +
        "manifest under Settings → App → Update server, which is how self-hosted forks ship their " +
        "own releases. Downloading an update is an action you trigger yourself and is subject to " +
        "GitHub's terms.\n" +
        "\n" +
        "5. Third-party providers.\n" +
        "When you send a request through the app, the provider you selected receives the prompt and " +
        "the data you configured for that request. Their processing is governed by their own privacy " +
        "policies, not by this one. You decide which providers you enable.\n" +
        "\n" +
        "6. The Docker image / server mode.\n" +
        "If you run the Docker image, you operate the server and you are the controller for the data " +
        "of your users. In multi-user mode the container writes accounts, sessions, keys and usage " +
        "data to the mounted data volume; passwords are stored as argon2id hashes. We receive " +
        "nothing. GitHub Pages hosts only the project website, never the app or its data.\n" +
        "\n" +
        "7. Your rights.\n" +
        "Where the GDPR applies you have the right of access (Art. 15), rectification (Art. 16), " +
        "erasure (Art. 17), restriction (Art. 18), portability (Art. 20) and objection (Art. 21). " +
        "Because we hold no data about you, the fastest way to exercise these rights is to delete " +
        "the local data; for anything else write to randompixxelstudios@gmail.com. You also have the " +
        "right to lodge a complaint with a supervisory authority (Art. 77), e.g. in your federal " +
        "state / EU member state. The competent authority for Baden-Württemberg is the " +
        "Landesbeauftragte für den Datenschutz und die Informationsfreiheit.\n" +
        "\n" +
        "8. US visitors (e.g. California/CCPA).\n" +
        "We sell no personal information and share none for cross-context behavioral advertising. The " +
        "only personal data we ever process is the contact data in the imprint, used solely to answer " +
        "your inquiry.\n" +
        "\n" +
        "9. Children.\n" +
        "This software is not directed at children under 16.\n" +
        "\n" +
        "10. Changes.\n" +
        "We update this policy when the software or the law changes. Status: September 2026."
    },
    {
      id: "terms",
      title: "Terms & disclaimer",
      body:
        "1. Subject.\n" +
        "Multi LLM is software (beta) that routes your API requests to third-party LLM providers and " +
        "exposes them through one OpenAI-compatible endpoint. It is a private, non-commercial project.\n" +
        "\n" +
        "2. The software.\n" +
        "You need your own provider accounts and API keys, and you pay their usage costs yourself. " +
        "The software is provided in beta: features may change, and provider behaviour (availability, " +
        "pricing, rate limits) is outside our control.\n" +
        "\n" +
        "3. No warranty; liability exclusion.\n" +
        "The software, the documentation, code samples and downloads are provided “as is”, without " +
        "warranty of accuracy, completeness, timeliness or fitness for a purpose. To the maximum " +
        "extent permitted by law, we exclude liability for damages arising from the use of – or " +
        "inability to use – the software or the described content. Nothing here limits liability that " +
        "cannot be limited by law (intent and gross negligence, injury to life, body or health, or " +
        "mandatory product-liability rules).\n" +
        "\n" +
        "4. What you are responsible for.\n" +
        "Multi LLM sits between you and third-party language models. It does not control what " +
        "those models answer and cannot know in advance what they will say. You are solely " +
        "responsible for how you use this software and for what happens as a result. That " +
        "includes the content you send through the proxy, the output the models return " +
        "(including anything unlawful, misleading, defamatory or harmful they produce), compliance " +
        "with the law in your jurisdiction and with the terms of the providers you enable, your " +
        "API keys and the charges they incur, and any decision you take on the basis of model " +
        "output. Decisions with legal, financial, medical or safety consequences must never " +
        "be based on model output alone.\n" +
        "\n" +
        "5. Compatibility and environment damage.\n" +
        "The author is not liable for damage caused by software or hardware that does not work " +
        "with this one, by defects in the environment it runs in, or by faults that would not have " +
        "occurred with a different configuration. This includes in particular: operating system " +
        "updates that change a WebView or a runtime library; missing or modified system libraries; " +
        "graphics and audio drivers; network stacks, VPNs and proxies; antivirus software and " +
        "firewalls blocking the local port; a missing or unstable system tray; ports already in " +
        "use by other programs; and container or virtualised environments that restrict local " +
        "ports or file paths.\n" +
        "\n" +
        "The author is likewise not liable for lost data, lost API keys, lost usage history or " +
        "lost configuration; for interrupted, duplicated or corrupted requests; for responses " +
        "that arrive incomplete or out of order; for costs a provider charges for requests that " +
        "failed; or for damage arising from continued use despite an error message. Test this " +
        "software before you depend on it, keep backups of your configuration, and check the " +
        "required system libraries before reporting a problem.\n" +
        "\n" +
        "Neither is the author liable for the content of any model response, for the actions or " +
        "omissions of any provider, for provider charges, or for any damage arising from your use " +
        "of this software.\n" +
        "\n" +
        "6. Your keys, your responsibility.\n" +
        "API keys you enter stay on your machine. You are responsible for keeping them secret and for " +
        "the charges they incur. If a key leaks, revoke it with the provider.\n" +
        "\n" +
        "7. Licence.\n" +
        "Multi LLM is proprietary software. All rights reserved: no permission is granted to use, " +
        "copy, modify, publish, distribute, sublicense or sell it without the written consent of " +
        "the copyright holder. The full text ships with every download and is available at " +
        "github.com/RandomPixelStudios/Multi-LLM. Third-party packages keep their own licences; " +
        "those are listed in the lock files. The licence covers the software, not this legal " +
        "text.\n" +
        "\n" +
        "8. Changes and law.\n" +
        "Content may change without notice. Governed by the laws of the Federal Republic of Germany; " +
        "if any provision is unenforceable, the rest remains in force."
    }
  ]
};
