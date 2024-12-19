# Status Codes

The request handler may return various status codes depending 
on the encountered errors. These codes provide insight into 
what went wrong, without necessarily indicating how to fix the issue.

## General Errors

| **Status Code**             | **Name**                                            | **Description**                                                                                      |
|-----------------------------|------------------------------------------------------|------------------------------------------------------------------------------------------------------|
| `400 BAD_REQUEST`           | `TapCoreError(SignatureError or ReceiptError::CheckFailure)` | The received Tap-related data is invalid (e.g., incorrect signature or failed receipt check).      |
| `402 PAYMENT_REQUIRED`      | `ReceiptNotFound`                                   | A required Tap receipt was not found in the request.                                                  |
| `402 PAYMENT_REQUIRED`      | `EscrowAccount`                                     | The signer does not match any known sender or the domain for signature recovery is incorrect (as per the `[blockchain]` section in the config). |
| `500 INTERNAL_SERVER_ERROR` | `TapCoreError(Other)`                               | An internal server error related to Tap core functionality, such as a failure in storing the receipt. |
| `502 BAD_GATEWAY`           | `SerializationError`                                | The response from `graph-node` could not be serialized into a GraphQL response.                      |
| `503 SERVICE_UNAVAILABLE`   | `QueryForwardingError`                              | The request could not be processed due to an error while forwarding the query to graph-node.     |

## Status Query Errors

These errors occur specifically during `status` operations:

| **Status Code**     | **Name**                          | **Description**                                  |
|---------------------|-----------------------------------|--------------------------------------------------|
| `400 BAD_REQUEST`   | `InvalidStatusQuery`              | The query contains invalid status parameters.     |
| `400 BAD_REQUEST`   | `UnsupportedStatusQueryFields`    | The query includes fields that are not supported. |
| `502 BAD_GATEWAY`   | `StatusQueryError`                | An internal error was encountered during the status query process. |
