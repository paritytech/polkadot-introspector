# Grafana dashboards

This directory holds different Grafana dashboard JSON templates.

For details about the Grafana dashboard JSON model, see: https://grafana.com/docs/grafana/latest/dashboards/build-dashboards/view-dashboard-json-model/

## Dashboards

### KVDB

See [kvbdb.json](./kvdb.json)

This dashboard defines three panels for:

- KVDB column key usage (in terms of storage space)
- KVDB column value usage (in terms of storage space)
- KVDB column entry (KV) count

Note that these are averages, as defined by the 