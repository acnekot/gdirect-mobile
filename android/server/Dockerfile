FROM python:3.12-alpine
WORKDIR /app
COPY token_server.py .
ENV GDIRECT_DATA=/data GDIRECT_HOST=0.0.0.0 GDIRECT_PORT=8787
VOLUME /data
EXPOSE 8787
ENTRYPOINT ["python3", "token_server.py"]
CMD ["serve"]
