# ZAP Scanning Report

ZAP by [Checkmarx](https://checkmarx.com/).


## Summary of Alerts

| Risk Level | Number of Alerts |
| --- | --- |
| High | 0 |
| Medium | 0 |
| Low | 0 |
| Informational | 1 |




## Insights

| Level | Reason | Site | Description | Statistic |
| --- | --- | --- | --- | --- |
| Low | Warning |  | ZAP warnings logged - see the zap.log file for details | 1    |
| Info | Informational | http://host.docker.internal:17401 | Percentage of responses with status code 2xx | 80 % |
| Info | Informational | http://host.docker.internal:17401 | Percentage of responses with status code 4xx | 20 % |
| Info | Informational | http://host.docker.internal:17401 | Percentage of endpoints with content type image/png | 33 % |
| Info | Informational | http://host.docker.internal:17401 | Percentage of endpoints with content type image/x-icon | 11 % |
| Info | Informational | http://host.docker.internal:17401 | Percentage of endpoints with content type text/css | 11 % |
| Info | Informational | http://host.docker.internal:17401 | Percentage of endpoints with content type text/html | 11 % |
| Info | Informational | http://host.docker.internal:17401 | Percentage of endpoints with content type text/javascript | 11 % |
| Info | Informational | http://host.docker.internal:17401 | Percentage of endpoints with content type text/plain | 22 % |
| Info | Informational | http://host.docker.internal:17401 | Percentage of endpoints with method GET | 100 % |
| Info | Informational | http://host.docker.internal:17401 | Count of total endpoints | 9    |







## Alerts

| Name | Risk Level | Number of Instances |
| --- | --- | --- |
| Modern Web Application | Informational | 1 |




## Alert Detail



### [ Modern Web Application ](https://www.zaproxy.org/docs/alerts/10109/)



##### Informational (Medium)

### Description

The application appears to be a modern web application. If you need to explore it automatically then the Client Spider may well be more effective than the standard one.

* URL: http://host.docker.internal:17401/
  * Node Name: `http://host.docker.internal:17401/`
  * Method: `GET`
  * Parameter: ``
  * Attack: ``
  * Evidence: `<script type="module" crossorigin src="/assets/index-BXjQ-Fus.js"></script>`
  * Other Info: `No links have been found while there are scripts, which is an indication that this is a modern web application.`


Instances: 1

### Solution

This is an informational alert and so no changes are required.

### Reference




#### Source ID: 3


