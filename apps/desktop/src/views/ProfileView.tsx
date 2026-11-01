import { useRef, useState } from "react";
import { useMutation, useQuery, useQueryClient } from "@tanstack/react-query";
import { ChipField, type ChipHandle } from "@/components/ChipField";
import { ErrorAlert } from "@/components/ErrorAlert";
import { PageHeading } from "@/components/PageHeading";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { ipc, type Profile, type ProfileBasic, type Stated, type WorkAuthorization, type WorkMode } from "@/lib/ipc";

const LIST_FIELDS = [
  ["countries", "Countries", "Add country"],
  ["regions", "Regions", "Add region"],
  ["cities", "Cities", "Add city"],
  ["currencies", "Currencies", "Add currency"],
  ["time_zones", "Time zones", "Add time zone"],
  ["languages", "Languages", "Add language"],
] as const;

type ListField = (typeof LIST_FIELDS)[number][0];

const MODES: [WorkMode, string][] = [
  ["remote", "Remote"],
  ["hybrid", "Hybrid"],
  ["onsite", "Onsite"],
];

const STATED: [Stated, string][] = [
  ["not_stated", "Not stated"],
  ["yes", "Yes"],
  ["no", "No"],
];

const blank = (country: string): WorkAuthorization => ({
  country,
  authorized: "not_stated",
  needs_sponsorship: "not_stated",
  note: "",
});

const same = (a: string, b: string) => a.toLowerCase() === b.toLowerCase();

function ProfileForm({
  profile,
  pending,
  onSave,
}: {
  profile: Profile;
  pending: boolean;
  onSave(basic: ProfileBasic, rows: WorkAuthorization[], removed: string[]): void;
}) {
  const locked = profile.locked;
  const addCountry = useRef<ChipHandle>(null);
  const [basic, setBasic] = useState(profile.basic);
  // Only what the user changed is written; a row nobody touched stays as it is.
  const [touched, setTouched] = useState<Record<string, WorkAuthorization>>({});
  const stored = (country: string) => profile.authorization.find((a) => a.country === country);
  const rowFor = (country: string) => touched[country] ?? stored(country) ?? blank(country);
  const countries = [
    ...basic.countries,
    ...profile.authorization.map((a) => a.country).filter((c) => !basic.countries.includes(c)),
  ];
  const emailBad = basic.email !== "" && !basic.email.includes("@");

  const addTo = (field: ListField) => (text: string) => {
    if (!basic[field].some((v) => same(v, text))) setBasic({ ...basic, [field]: [...basic[field], text] });
    return null;
  };
  const removeFrom = (field: ListField) => (index: number) =>
    setBasic({ ...basic, [field]: basic[field].filter((_, i) => i !== index) });
  const edit = (country: string, change: Partial<WorkAuthorization>) =>
    setTouched({ ...touched, [country]: { ...rowFor(country), ...change } });

  const submit = () => {
    const rows = Object.values(touched).filter((r) => basic.countries.includes(r.country));
    const removed = profile.authorization.map((a) => a.country).filter((c) => !basic.countries.includes(c));
    onSave(basic, rows, removed);
  };

  return (
    <form
      className="grid gap-4"
      onSubmit={(e) => {
        e.preventDefault();
        if (!emailBad) submit();
      }}
    >
      <div className="flex items-center justify-between">
        <PageHeading>Profile</PageHeading>
        <Button type="submit" disabled={pending || emailBad}>
          {pending ? "Saving..." : "Save"}
        </Button>
      </div>
      <section aria-label="Basics" className="grid gap-3 rounded-lg border p-4">
        <div className="grid grid-cols-2 gap-4">
          <div className="grid gap-1.5">
            <Label htmlFor="profile-name">Name</Label>
            <Input
              id="profile-name"
              value={basic.name}
              onChange={(e) => setBasic({ ...basic, name: e.target.value })}
            />
          </div>
          <div className="grid gap-1.5">
            <Label htmlFor="profile-email">Email</Label>
            <Input
              id="profile-email"
              value={basic.email}
              aria-invalid={emailBad || undefined}
              aria-describedby={emailBad ? "profile-email-error" : undefined}
              onChange={(e) => setBasic({ ...basic, email: e.target.value })}
            />
            {emailBad && (
              <p id="profile-email-error" className="text-xs text-destructive">
                An email address has an @ in it.
              </p>
            )}
          </div>
        </div>
        {LIST_FIELDS.map(([field, label, addLabel]) => (
          <ChipField
            key={field}
            ref={field === "countries" ? addCountry : undefined}
            label={label}
            addLabel={addLabel}
            values={basic[field]}
            onAdd={addTo(field)}
            onRemove={removeFrom(field)}
          />
        ))}
        <fieldset className="grid grid-cols-[10rem_1fr] items-center gap-x-4">
          <legend className="sr-only">Work mode</legend>
          <span className="text-sm text-muted-foreground">Work mode</span>
          <div className="flex gap-4">
            {MODES.map(([mode, label]) => (
              <label key={mode} className="flex items-center gap-1.5 text-sm">
                <input
                  type="checkbox"
                  checked={basic.work_modes.includes(mode)}
                  onChange={(e) =>
                    setBasic({
                      ...basic,
                      work_modes: e.target.checked
                        ? [...basic.work_modes, mode]
                        : basic.work_modes.filter((m) => m !== mode),
                    })
                  }
                />
                {label}
              </label>
            ))}
          </div>
        </fieldset>
        <ChipField
          label="Employment"
          addLabel="Add employment"
          placeholder="Contract, PT"
          values={basic.employment_types.map((e) => `${e.kind} (${e.country})`)}
          onAdd={(text) => {
            const cut = text.lastIndexOf(",");
            const kind = text.slice(0, cut).trim();
            const country = text.slice(cut + 1).trim();
            const known = basic.countries.find((c) => same(c, country));
            if (cut < 0 || !kind || !known) {
              return "Write the kind and one of your countries, like: Contract, PT";
            }
            setBasic({ ...basic, employment_types: [...basic.employment_types, { kind, country: known }] });
            return null;
          }}
          onRemove={(i) =>
            setBasic({ ...basic, employment_types: basic.employment_types.filter((_, n) => n !== i) })
          }
        />
      </section>

      <section aria-labelledby="authorization" className="grid gap-3 rounded-lg border p-4">
        <h2 id="authorization" className="font-medium">
          Work authorization and sponsorship
        </h2>
        <p className="text-sm">
          You enter this yourself. JobPilot never guesses it, never fills it in for you, and never
          works it out from your citizenship, location, or resume.
        </p>
        {countries.length === 0 ? (
          <p className="text-sm text-muted-foreground">No country added yet.</p>
        ) : (
          <table className="w-full text-sm">
            <thead>
              <tr className="text-left text-muted-foreground">
                <th className="py-1 font-normal">Country</th>
                <th className="py-1 font-normal">What you state</th>
                <th className="py-1 font-normal">Needs sponsorship</th>
                <th className="py-1 font-normal">Free note (yours, optional)</th>
              </tr>
            </thead>
            <tbody>
              {countries.map((country) => {
                const row = rowFor(country);
                return (
                  <tr key={country} aria-label={country}>
                    <th scope="row" className="py-1 text-left font-normal">
                      {country}
                    </th>
                    {locked ? (
                      <td colSpan={3} className="py-1">
                        <span className="flex items-center gap-2">
                          <select disabled aria-label={`${country}: what you state`} className="h-8 rounded-lg border px-2">
                            <option>Locked</option>
                          </select>
                          <select disabled aria-label={`${country}: needs sponsorship`} className="h-8 rounded-lg border px-2">
                            <option>Locked</option>
                          </select>
                          <span className="text-muted-foreground">Locked, see Recovery</span>
                        </span>
                      </td>
                    ) : (
                      <>
                        {(["authorized", "needs_sponsorship"] as const).map((key) => (
                          <td key={key} className="py-1">
                            <select
                              aria-label={`${country}: ${key === "authorized" ? "what you state" : "needs sponsorship"}`}
                              className="h-8 rounded-lg border bg-background px-2"
                              value={row[key]}
                              onChange={(e) => edit(country, { [key]: e.target.value as Stated })}
                            >
                              {STATED.map(([value, label]) => (
                                <option key={value} value={value}>
                                  {label}
                                </option>
                              ))}
                            </select>
                          </td>
                        ))}
                        <td className="py-1">
                          <Input
                            aria-label={`${country}: note`}
                            value={row.note}
                            onChange={(e) => edit(country, { note: e.target.value })}
                          />
                        </td>
                      </>
                    )}
                  </tr>
                );
              })}
            </tbody>
          </table>
        )}
        {locked && (
          <p className="text-sm text-muted-foreground">
            These rows cannot be read or changed until the key is back. See Locked fields on the
            Data screen.
          </p>
        )}
        <div>
          <Button
            type="button"
            variant="outline"
            size="sm"
            onClick={() => addCountry.current?.open()}
          >
            Add a country
          </Button>
        </div>
      </section>
    </form>
  );
}

export function ProfileView() {
  const client = useQueryClient();
  const profile = useQuery({ queryKey: ["profile"], queryFn: ipc.profileGet });
  const save = useMutation({
    mutationFn: async (v: { basic: ProfileBasic; rows: WorkAuthorization[]; removed: string[] }) => {
      await ipc.profileSetBasic(v.basic);
      // While locked the authorization rows stay ciphertext: nothing is sent for them.
      if (!profile.data?.locked) {
        for (const row of v.rows) await ipc.profileSetAuthorization(row);
        for (const country of v.removed) await ipc.profileRemoveAuthorization(country);
      }
    },
    onSuccess: () => client.resetQueries({ queryKey: ["profile"] }),
  });
  return (
    <div className="grid max-w-4xl gap-4">
      <ErrorAlert error={save.error ?? profile.error} />
      {save.isSuccess && (
        <p role="status" className="text-sm">
          Saved.
        </p>
      )}
      {profile.data ? (
        <ProfileForm
          profile={profile.data}
          pending={save.isPending}
          onSave={(basic, rows, removed) => save.mutate({ basic, rows, removed })}
        />
      ) : (
        <>
          <PageHeading>Profile</PageHeading>
          {profile.isPending && <p className="text-sm text-muted-foreground">Loading...</p>}
        </>
      )}
    </div>
  );
}
