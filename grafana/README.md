# Grafana dashboards

This directory holds different Grafana dashboard JSON models. For details about the Grafana dashboard JSON model, see: https://grafana.com/docs/grafana/latest/dashboards/build-dashboards/view-dashboard-json-model/

In case you want to import these dashboards to Grafana, you can do so by following these steps:
https://grafana.com/docs/grafana/v9.0/dashboards/export-import/. It is worthwhile mentioning that importing dashboards into Grafana requires Editor rights as a Grafana user.

## Monitoring architecture

Polkadot introspector is currently deployed in Versi. Versi is a test network which runs both a relay chain and several parachains in a Kubernetes environment. 

## Dashboards

This section holds some details on the specific dashboards we have created with Polkadot introspector.

### KVDB

See [kvbdb.json](./kvdb.json)

This dashboard defines three panels for:

- KVDB column key usage (in terms of storage space)
- KVDB column value usage (in terms of storage space)
- KVDB column entry (KV) count

Note that these are averages (`avg` as defined by the Prometheus queries). You can read more about Prometheus queries here: https://prometheus.io/docs/prometheus/latest/querying/basics/. Some descriptions are provided below as well.
