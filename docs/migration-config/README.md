# Migration guide

In case you want to migrate from the old stack into the new
stack quickly, you can use the following two configurations
to help you migrate without changing too many fields.

You just need to drop the config, use it in the `--config`
args and add some additional environment variables.

Please take a deeper look at the config, but 90% of the time
you'll be safe just using it. If you find any issues using
this config, feel free to open an issue.

## Missing configuration fields

The new stack requires adding an escrow subgraph,
similar to the current network subgraph field.

You can edit the config yourself and add the following fields:

```
[subgraphs.escrow]
query_url = "<escrow subgraph query url>"
deployment_id = "<escrow deployment id>"
```

You can also add these environment variables in your new setup:

```
INDEXER_SUBGRAPHS__ESCROW__QUERY_URL=<escrow subgraph query url>
INDEXER_SUBGRAPHS__ESCROW__DEPLOYMENT_ID=<escrow deployment id>
```

Users typically have an easier time using the config over 
environment variables, we recommend moving your environment 
variables to plain text inside the config over time.

## Booleans inside config

The configuration doesn't accept booleans as strings, so in
case you had `INDEXER_SERVICE_SERVE_NETWORK_SUBGRAPH` environment
variable, please update to `INDEXER_SERVICE__SERVE_NETWORK_SUBGRAPH`
and use `true/false` as values (or you could update directly in
the configuration).

Please update the config accordingly. Also, check out the
explanation for each field in
[config/minimal-config-example.toml](config/minimal-config-example.toml)
and also [config/maximal-config-example.toml](config/maximal-config-example.toml)
