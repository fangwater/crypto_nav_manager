import { CircleHelp } from 'lucide-react'
import { useId, useState } from 'react'
import {
  ANALYSIS_METRIC_HELP,
  toggleAnalysisHelperOpen,
  type AnalysisMetricHelpItem,
} from '../analysisMetricHelp'

export function AnalysisMetricHelper({
  items = ANALYSIS_METRIC_HELP,
}: {
  items?: readonly AnalysisMetricHelpItem[]
}) {
  const [open, setOpen] = useState(false)
  const panelId = useId()

  return (
    <div className="analysis-metric-helper">
      <button
        type="button"
        className="analysis-metric-helper__toggle"
        aria-expanded={open}
        aria-controls={panelId}
        onClick={() => setOpen((current) => toggleAnalysisHelperOpen(current))}
      >
        <CircleHelp size={14} />
        指标计算说明
      </button>
      {open && (
        <div
          className="analysis-metric-helper__panel"
          id={panelId}
          role="region"
          aria-label="指标计算说明"
        >
          {items.map((item) => (
            <article key={item.id}>
              <h3>{item.title}</h3>
              <p>{item.formula}</p>
              <small>{item.meaning}</small>
            </article>
          ))}
        </div>
      )}
    </div>
  )
}
