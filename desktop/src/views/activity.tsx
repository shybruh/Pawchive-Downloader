import { DownloadIcon, FolderOpenIcon, TrashIcon } from "lucide-react";
import { call } from "@/lib/engine";
import { Button } from "@/components/ui";
import { PageHeader } from "@/components/page";
import { LogList } from "@/components/log-list";

export function ActivityView() {
  return (
    <>
      <PageHeader title="Activity log" description="Everything the engine is doing, live."
        actions={<>
          <Button variant="ghost" size="sm" onClick={() => call("appBridge", "openLogsFolder")}><FolderOpenIcon />Log files</Button>
          <Button variant="outline" size="sm" onClick={() => call("appBridge", "exportLogs")}><DownloadIcon />Export</Button>
          <Button variant="outline" size="sm" onClick={() => call("appBridge", "clearLogs")}><TrashIcon />Clear</Button>
        </>} />
      <div className="flex min-h-0 flex-1 flex-col px-6 py-4"><LogList className="flex-1" /></div>
    </>
  );
}
