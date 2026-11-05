import { useState } from "react";
import { FileTextIcon, PlusIcon, TagsIcon, XIcon } from "lucide-react";
import { call, useModel, useSetting } from "@/lib/engine";
import { Button, Card, CardContent, CardDescription, CardHeader, CardTitle, Empty, Input, Select, Switch } from "@/components/ui";
import { PageBody, PageHeader } from "@/components/page";

export function KnownView() {
  const { rows } = useModel<{ name: string; display: string }>("appBridge.knownModel");
  const [name, setName] = useState("");
  const [mode, setMode] = useSetting<string>("appBridge", "knownRecognitionMode", "hybrid");
  const [separate, setSeparate] = useSetting<boolean>("appBridge", "separateFoldersByKnown", false);

  const add = () => { const n = name.trim(); if (n) { call("appBridge", "addKnownCharacter", n); setName(""); } };

  return (
    <>
      <PageHeader title="Known series" description="Characters and series used to sort downloads into folders."
        actions={<Button variant="outline" size="sm" onClick={() => call("appBridge", "openKnownTxt")}><FileTextIcon />Edit as text</Button>} />
      <PageBody className="flex flex-col gap-5">
        <Card>
          <CardHeader>
            <CardTitle>Recognition</CardTitle>
            <CardDescription>How downloads are matched against this list.</CardDescription>
          </CardHeader>
          <CardContent className="flex flex-wrap items-center gap-6">
            <Select value={mode} onValueChange={setMode} options={[
              { value: "hybrid", label: "Hybrid (list + learning)" },
              { value: "database_only", label: "This list only" },
              { value: "learning_only", label: "Learning only" },
            ]} />
            <label className="flex items-center gap-2 text-sm">
              <Switch checked={!!separate} onCheckedChange={setSeparate} />
              Put matches in their own folders
            </label>
          </CardContent>
        </Card>

        <div className="flex gap-2">
          <Input placeholder="Add a character or series, e.g. Hatsune Miku" value={name} onChange={(e) => setName(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && add()} />
          <Button onClick={add} disabled={!name.trim()}><PlusIcon />Add</Button>
        </div>

        {rows.length === 0 ? (
          <Empty icon={TagsIcon} title="The list is empty" description="Add the characters or series you collect." />
        ) : (
          <div className="flex flex-wrap gap-2">
            {rows.map((r, i) => (
              <span key={`${r.name}-${i}`} className="group inline-flex items-center gap-1 rounded-md border bg-card py-1 pr-1 pl-2.5 text-sm">
                {r.display || r.name}
                <button className="rounded p-0.5 text-muted-foreground hover:bg-accent hover:text-foreground" onClick={() => call("appBridge", "removeKnownCharacter", i)}>
                  <XIcon className="size-3.5" />
                </button>
              </span>
            ))}
          </div>
        )}
      </PageBody>
    </>
  );
}
