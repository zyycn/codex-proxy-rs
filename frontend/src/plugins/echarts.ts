import { BarChart, LineChart, PieChart } from 'echarts/charts'
import { GridComponent, LegendPlainComponent, TooltipComponent } from 'echarts/components'
import { use } from 'echarts/core'
import { CanvasRenderer } from 'echarts/renderers'

use([
  BarChart,
  LineChart,
  PieChart,
  GridComponent,
  LegendPlainComponent,
  TooltipComponent,
  CanvasRenderer,
])
